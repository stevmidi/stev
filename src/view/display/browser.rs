//! The browser side panel: a toggleable, fixed-width panel on the left with
//! two categories. **Projects** holds the project folders under
//! `paths::project_dir()`, each expandable to its `.stev` and `.mid` files,
//! then the loose files at the projects root. **Plugins** (macOS only, where
//! the plugin host runs) lists every installed instrument plugin: Enter puts
//! the selected one on the selected track, a drag onto a track puts it there.
//! The panel replaced the ⌘O Project modal. `⌘⌥B` toggles it, `⌘O` shows it.
//!
//! Its keyboard focus is view-local: while focused it takes the arrows,
//! Enter, Delete/Backspace and Esc, and every other key falls through to the
//! view underneath (`Arranger` or `Clip`), so Space still plays. The main
//! canvas is drawn shifted right by the panel's width (an egui layer
//! transform) and the pointer is shifted to match (`InputPoller`), so none of
//! the canvas's own layout knows the panel exists — a canvas x below `0` is
//! the panel. See `020-views-and-state.md` § Views and `030-ui-design.md`
//! § Browser Panel.
//!
//! [`BrowserTree`] is the pure tree model (rows, selection, expand/collapse,
//! what Enter means), unit-tested; the `impl Display` below drives it from
//! input and reads the filesystem.

use std::{collections::HashSet, path::PathBuf};

use egui::{Key, Pos2};

use crate::core::{
    project::{
        FolderListing, ProjectAction, delete_project, list_folder, list_project_folders,
        midi_file_path,
    },
    settings::update_settings,
};

use super::*;

/// The panel's width, in points. Whole points, so the shifted canvas keeps
/// its pixel-snapped seams.
pub(super) const BROWSER_W: f32 = 260.0;
/// Height of one row of the tree.
pub(super) const BROWSER_ROW_H: f32 = 22.0;
/// Top of the first row, below the category heading.
pub(super) const BROWSER_LIST_TOP: f32 = theme::HEADER_H + 6.0;
/// Width of the disclosure triangle's hit zone, from a folder or category
/// row's indent — a click there toggles it rather than only selecting it.
pub(super) const BROWSER_DISCLOSURE_W: f32 = 26.0;
/// Indent per tree depth.
pub(super) const BROWSER_INDENT_X: f32 = 14.0;

/// A top-level category of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BrowserCategory {
    /// The project folders and their files.
    Projects,
    /// The installed instrument plugins.
    Plugins,
}

impl BrowserCategory {
    /// The category row's label.
    pub(super) fn label(self) -> &'static str {
        match self {
            BrowserCategory::Projects => "PROJECTS",
            BrowserCategory::Plugins => "PLUGINS",
        }
    }
}

/// An installed instrument plugin as the Plugins category lists it: a
/// format-free copy of the catalog entry's identity, so the tree stays pure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BrowserPlugin {
    /// Display name.
    pub(super) name: String,
    /// The format's short label (`CLAP`, `VST3`), shown dim after the name.
    pub(super) format: &'static str,
    /// Path to the plugin bundle.
    pub(super) bundle_path: PathBuf,
    /// Plugin id within the bundle.
    pub(super) plugin_id: String,
}

/// What the Plugins category lists: the plugins found so far, in catalog
/// order, and whether the background scan is still finding more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PluginListing {
    /// The plugins, in the order given.
    plugins: Vec<BrowserPlugin>,
    /// Whether the catalog scan is still running.
    scanning: bool,
}

impl PluginListing {
    /// `plugins` as listed — the catalog already sorts by name, ignoring
    /// case, then format, so the same plugin as CLAP and VST3 sits side by
    /// side (`plugin_host::catalog`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn new(plugins: Vec<BrowserPlugin>, scanning: bool) -> Self {
        Self { plugins, scanning }
    }
}

/// One entry of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BrowserItem {
    /// A category heading, expandable like a folder.
    Category(BrowserCategory),
    /// A project folder.
    Folder(String),
    /// A `.stev` project, in `folder` (`None`: the projects root).
    Project {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The project name (file stem).
        name: String,
    },
    /// A `.mid` file, in `folder` (`None`: the projects root). Enter imports
    /// it on the selected track at the cursor; dragged out of the panel, it
    /// imports where it is dropped (`input/midi_drag.rs`).
    MidiFile {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The file stem.
        name: String,
    },
    /// An installed instrument plugin. Enter puts it on the selected track;
    /// dragged out of the panel, on the track it is dropped on
    /// (`input/plugin_drag.rs`).
    Plugin(BrowserPlugin),
    /// The inert "Scanning…" row ending the Plugins category while the
    /// catalog scan runs.
    Scanning,
}

impl BrowserItem {
    /// The row Left steps out to: a file's folder (or the Projects category
    /// at the projects root), a folder's or a plugin's category. A category
    /// has none.
    fn parent(&self) -> Option<BrowserItem> {
        match self {
            BrowserItem::Category(_) => None,
            BrowserItem::Project { folder, .. } | BrowserItem::MidiFile { folder, .. } => {
                Some(folder.clone().map_or(
                    BrowserItem::Category(BrowserCategory::Projects),
                    BrowserItem::Folder,
                ))
            }
            BrowserItem::Folder(_) => Some(BrowserItem::Category(BrowserCategory::Projects)),
            BrowserItem::Plugin(_) | BrowserItem::Scanning => {
                Some(BrowserItem::Category(BrowserCategory::Plugins))
            }
        }
    }
}

/// One visible row: the item and its indent depth (`0` for a category, `1`
/// for what sits directly under one, `2` inside a project folder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BrowserRow {
    /// What the row shows.
    pub(super) item: BrowserItem,
    /// Indent depth.
    pub(super) depth: u8,
}

/// What Enter (or a double-click) on the selected row asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BrowserAction {
    /// Open this project.
    OpenProject {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The project name.
        name: String,
    },
    /// Make this folder the current project folder.
    SetCurrentFolder(String),
    /// Put this plugin on the selected track.
    LoadPlugin(BrowserPlugin),
    /// Import this `.mid` on the selected track at the cursor.
    ImportMidi {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The file stem.
        name: String,
    },
    /// Delete this project (the confirmed second Enter).
    DeleteProject {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The project name.
        name: String,
    },
}

/// The tree: what is on disk (refreshed whenever the panel is shown and
/// after a delete), the plugins (refreshed as the catalog scan reports),
/// which categories and folders are expanded and which row is selected.
/// Expansion and selection survive a refresh and hiding the panel; neither is
/// persisted. Both categories start expanded, every folder collapsed.
#[derive(Default)]
pub(super) struct BrowserTree {
    /// Each project folder with its listing, alphabetical.
    folders: Vec<(String, FolderListing)>,
    /// The loose files at the projects root.
    root: FolderListing,
    /// The Plugins category's contents; `None` leaves the category out (no
    /// plugin host).
    plugins: Option<PluginListing>,
    /// The collapsed categories.
    collapsed: HashSet<BrowserCategory>,
    /// Names of the expanded folders.
    expanded: HashSet<String>,
    /// The visible rows, derived from the fields above by
    /// [`rebuild_rows`](Self::rebuild_rows) whenever one of them changes —
    /// read every frame, so not rebuilt per read.
    rows: Vec<BrowserRow>,
    /// The selected item, `None` until something is selected.
    selected: Option<BrowserItem>,
    /// Whether the selected project awaits its delete confirm (the second
    /// Enter). Any selection change disarms it.
    pub(super) delete_armed: bool,
}

/// Appends `listing`'s projects then MIDI files, in `folder` (`None`: the
/// projects root), as rows at `depth`.
fn push_file_rows(
    rows: &mut Vec<BrowserRow>,
    folder: Option<&str>,
    listing: &FolderListing,
    depth: u8,
) {
    let folder = folder.map(str::to_owned);
    for name in &listing.projects {
        rows.push(BrowserRow {
            item: BrowserItem::Project {
                folder: folder.clone(),
                name: name.clone(),
            },
            depth,
        });
    }
    for name in &listing.midi_files {
        rows.push(BrowserRow {
            item: BrowserItem::MidiFile {
                folder: folder.clone(),
                name: name.clone(),
            },
            depth,
        });
    }
}

impl BrowserTree {
    /// Replaces what the tree lists, keeping expansion and selection. A
    /// selected item that is gone hands the selection to the row now at its
    /// old index (clamped), so a delete lands on the next row.
    pub(super) fn reload(&mut self, folders: Vec<(String, FolderListing)>, root: FolderListing) {
        let old_index = self.selected_index();
        self.folders = folders;
        self.root = root;
        self.expanded
            .retain(|name| self.folders.iter().any(|(f, _)| f == name));
        self.delete_armed = false;
        self.relist(old_index);
    }

    /// Replaces the Plugins category's contents (`None`: no category),
    /// keeping the selection like [`reload`](Self::reload).
    pub(super) fn set_plugins(&mut self, plugins: Option<PluginListing>) {
        if self.plugins == plugins {
            return;
        }
        let old_index = self.selected_index();
        self.plugins = plugins;
        self.relist(old_index);
    }

    /// Rebuilds the rows after what they list changed. A selected item that
    /// is gone hands the selection to the row now at `old_index` (clamped).
    fn relist(&mut self, old_index: Option<usize>) {
        self.rebuild_rows();
        if self
            .selected
            .as_ref()
            .is_some_and(|sel| !self.is_listed(sel))
        {
            let last = self.rows.len().saturating_sub(1);
            self.selected = old_index
                .and_then(|idx| self.rows.get(idx.min(last)))
                .map(|row| row.item.clone());
        }
    }

    /// Re-derives [`rows`](Self::rows). Under **Projects** (when expanded):
    /// each folder, followed (when expanded) by its projects newest first and
    /// its MIDI files; then the projects root's loose files. Under
    /// **Plugins** (when listed and expanded): the plugins, then
    /// "Scanning…" while the scan runs.
    fn rebuild_rows(&mut self) {
        let mut rows = vec![BrowserRow {
            item: BrowserItem::Category(BrowserCategory::Projects),
            depth: 0,
        }];
        if self.is_category_expanded(BrowserCategory::Projects) {
            for (name, listing) in &self.folders {
                rows.push(BrowserRow {
                    item: BrowserItem::Folder(name.clone()),
                    depth: 1,
                });
                if self.expanded.contains(name) {
                    push_file_rows(&mut rows, Some(name), listing, 2);
                }
            }
            push_file_rows(&mut rows, None, &self.root, 1);
        }
        if let Some(listing) = &self.plugins {
            rows.push(BrowserRow {
                item: BrowserItem::Category(BrowserCategory::Plugins),
                depth: 0,
            });
            if self.is_category_expanded(BrowserCategory::Plugins) {
                let plugins = listing.plugins.iter().cloned().map(BrowserItem::Plugin);
                let scanning = listing.scanning.then_some(BrowserItem::Scanning);
                rows.extend(
                    plugins
                        .chain(scanning)
                        .map(|item| BrowserRow { item, depth: 1 }),
                );
            }
        }
        self.rows = rows;
    }

    /// The visible rows, top to bottom.
    pub(super) fn rows(&self) -> &[BrowserRow] {
        &self.rows
    }

    /// Whether `item` is one of the visible rows.
    fn is_listed(&self, item: &BrowserItem) -> bool {
        self.rows.iter().any(|row| &row.item == item)
    }

    /// The selected item.
    pub(super) fn selected(&self) -> Option<&BrowserItem> {
        self.selected.as_ref()
    }

    /// The selected row's index in [`rows`](Self::rows).
    pub(super) fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| &row.item == selected)
    }

    /// Whether `folder` is expanded.
    fn is_expanded(&self, folder: &str) -> bool {
        self.expanded.contains(folder)
    }

    /// Whether `category` is expanded.
    fn is_category_expanded(&self, category: BrowserCategory) -> bool {
        !self.collapsed.contains(&category)
    }

    /// Whether `item` is an expanded folder or category; `None` for a row
    /// that doesn't expand.
    pub(super) fn item_expanded(&self, item: &BrowserItem) -> Option<bool> {
        match item {
            BrowserItem::Category(category) => Some(self.is_category_expanded(*category)),
            BrowserItem::Folder(name) => Some(self.is_expanded(name)),
            _ => None,
        }
    }

    /// Expands or collapses `item`, a folder or a category; anything else is
    /// left alone.
    fn set_item_expanded(&mut self, item: &BrowserItem, expanded: bool) {
        let changed = match item {
            BrowserItem::Category(category) if expanded => self.collapsed.remove(category),
            BrowserItem::Category(category) => self.collapsed.insert(*category),
            BrowserItem::Folder(name) if expanded => self.expanded.insert(name.clone()),
            BrowserItem::Folder(name) => self.expanded.remove(name),
            _ => false,
        };
        if changed {
            self.rebuild_rows();
        }
    }

    /// Selects `item` and disarms a pending delete.
    pub(super) fn select(&mut self, item: BrowserItem) {
        self.selected = Some(item);
        self.delete_armed = false;
    }

    /// The first time the panel shows (nothing selected yet): expands the
    /// current folder and selects the open project if it is listed, else the
    /// current folder, else the first row.
    pub(super) fn select_initial(
        &mut self,
        current_folder: Option<&str>,
        current_name: Option<&str>,
    ) {
        if self.selected.is_some() {
            return;
        }
        if let Some(folder) = current_folder
            && self.folders.iter().any(|(f, _)| f == folder)
        {
            self.set_item_expanded(&BrowserItem::Folder(folder.to_owned()), true);
        }
        let open = current_name.map(|name| BrowserItem::Project {
            folder: current_folder.map(str::to_owned),
            name: name.to_owned(),
        });
        let folder = current_folder.map(|f| BrowserItem::Folder(f.to_owned()));
        self.selected = [open, folder]
            .into_iter()
            .flatten()
            .find(|item| self.is_listed(item))
            .or_else(|| self.rows.first().map(|row| row.item.clone()));
    }

    /// Moves the selection `delta` rows, clamped to the list; with nothing
    /// selected, selects the first row.
    pub(super) fn move_selection(&mut self, delta: i32) {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let idx = match self.selected_index() {
            Some(idx) => (idx as i64 + delta as i64).clamp(0, last as i64) as usize,
            None => 0,
        };
        self.select(self.rows[idx].item.clone());
    }

    /// Right: expands a collapsed folder or category, or steps into an
    /// expanded one's first entry. Nothing on any other row.
    pub(super) fn expand(&mut self) {
        let Some(item) = self.selected.clone() else {
            return;
        };
        match self.item_expanded(&item) {
            Some(true) => self.move_selection(1),
            Some(false) => self.set_item_expanded(&item, true),
            None => {}
        }
    }

    /// Left: collapses an expanded folder or category; on anything else,
    /// selects the row it sits under.
    pub(super) fn collapse(&mut self) {
        let Some(item) = self.selected.clone() else {
            return;
        };
        if self.item_expanded(&item) == Some(true) {
            self.set_item_expanded(&item, false);
        } else if let Some(parent) = item.parent() {
            self.select(parent);
        }
    }

    /// Expands or collapses `item`, a folder or a category (a click on its
    /// disclosure triangle).
    pub(super) fn toggle(&mut self, item: &BrowserItem) {
        if let Some(expanded) = self.item_expanded(item) {
            self.set_item_expanded(item, !expanded);
        }
    }

    /// Enter: confirms a pending delete; else opens the selected project,
    /// makes the selected folder current (and expands it), imports the
    /// selected MIDI file, puts the selected plugin on the selected track,
    /// or expands/collapses a category.
    pub(super) fn activate(&mut self) -> Option<BrowserAction> {
        let armed = std::mem::take(&mut self.delete_armed);
        let item = self.selected.clone()?;
        match item {
            BrowserItem::Project { folder, name } if armed => {
                Some(BrowserAction::DeleteProject { folder, name })
            }
            BrowserItem::Category(_) => {
                self.toggle(&item);
                None
            }
            BrowserItem::Folder(ref name) => {
                self.set_item_expanded(&item, true);
                Some(BrowserAction::SetCurrentFolder(name.clone()))
            }
            BrowserItem::Plugin(plugin) => Some(BrowserAction::LoadPlugin(plugin)),
            BrowserItem::Scanning => None,
            BrowserItem::Project { folder, name } => {
                Some(BrowserAction::OpenProject { folder, name })
            }
            BrowserItem::MidiFile { folder, name } => {
                Some(BrowserAction::ImportMidi { folder, name })
            }
        }
    }

    /// Delete/Backspace: arms the delete confirm on a selected project.
    pub(super) fn arm_delete(&mut self) {
        self.delete_armed = matches!(self.selected, Some(BrowserItem::Project { .. }));
    }
}

/// The first visible row after scrolling so the row `selected` is on screen,
/// moving `scroll` as little as possible; `visible` rows fit.
pub(super) fn scroll_to_show(scroll: usize, selected: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    if selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

/// The panel's view-local state, grouped out of [`Display`] as
/// `Display::browser`.
#[derive(Default)]
pub(super) struct BrowserPanel {
    /// Shown or hidden. Hidden at startup.
    pub(super) visible: bool,
    /// The tree.
    pub(super) tree: BrowserTree,
    /// The first visible row.
    pub(super) scroll: usize,
    /// A draggable row the primary button went down on — a `.mid` file or a
    /// Plugins row — `Some` until the release: dragged out of the panel, it
    /// becomes the import drag (`GestureState::midi_drag`) or the plugin
    /// drag (`GestureState::plugin_drag`).
    pub(super) press: Option<BrowserPress>,
}

/// A draggable browser row under the held primary button, before it leaves
/// the panel.
pub(super) struct BrowserPress {
    /// The pressed row.
    pub(super) item: BrowserItem,
    /// Canvas-space press point.
    pub(super) origin: (f32, f32),
    /// Latched once the pointer has moved `DRAG_THRESHOLD_PX` off `origin`:
    /// the press is a drag from then on, and the pointer shows the closed
    /// hand while it is still over the panel — a plain click never does.
    pub(super) dragging: bool,
}

impl Display {
    /// The panel's width this frame: [`BROWSER_W`] while shown, else `0`.
    pub(super) fn browser_width(&self) -> f32 {
        if self.browser.visible { BROWSER_W } else { 0.0 }
    }

    /// The canvas: the window less the panel's width, in canvas space (its
    /// left edge at the window's — the canvas is drawn shifted right by the
    /// panel's width).
    pub(super) fn sync_canvas_rect(&mut self, window: Rect) {
        let mut canvas = window;
        canvas.max.x -= self.browser_width();
        self.render.canvas_rect = canvas;
    }

    /// The pointer in canvas space, as this frame's `InputPoller::poll` saw
    /// it (the shift lives there only); `None` with no pointer over the
    /// window.
    pub(super) fn canvas_pointer(&self) -> Option<Pos2> {
        self.input_poller.pointer_pos()
    }

    /// Whether canvas-space `x` is over the panel: everything left of the
    /// canvas is.
    pub(super) fn is_over_browser(x: f32) -> bool {
        x < 0.0
    }

    /// How many rows fit in the panel this frame.
    fn browser_visible_rows(&self) -> usize {
        let list_h = self.render.canvas_rect.height() - BROWSER_LIST_TOP - theme::STATUS_H;
        (list_h / BROWSER_ROW_H).floor().max(1.0) as usize
    }

    /// Re-lists the Plugins category from the plugin catalog — on showing
    /// the panel and whenever the background scan reports. Off macOS there
    /// is no plugin host and no category.
    pub(super) fn sync_browser_plugins(&mut self) {
        #[cfg(target_os = "macos")]
        let listing = Some(PluginListing::new(
            self.plugin_catalog()
                .iter()
                .map(|entry| BrowserPlugin {
                    name: entry.name.clone(),
                    format: entry.format.label(),
                    bundle_path: entry.bundle_path.clone(),
                    plugin_id: entry.plugin_id.clone(),
                })
                .collect(),
            self.plugin_catalog_scanning(),
        ));
        #[cfg(not(target_os = "macos"))]
        let listing = None;
        self.browser.tree.set_plugins(listing);
    }

    /// Re-reads the projects tree from disk.
    pub(super) fn reload_browser(&mut self) {
        let folders = list_project_folders()
            .into_iter()
            .map(|name| {
                let listing = list_folder(Some(&name));
                (name, listing)
            })
            .collect();
        self.browser.tree.reload(folders, list_folder(None));
    }

    /// Shows the panel with the keyboard (`⌘⌥B` on a hidden panel, `⌘O`),
    /// re-reading the tree from disk.
    pub(super) fn show_browser(&mut self) {
        self.reload_browser();
        self.sync_browser_plugins();
        self.browser.tree.select_initial(
            self.project.project_current_folder.as_deref(),
            self.project.project_current_name.as_deref(),
        );
        self.browser.visible = true;
        self.key_focus = KeyFocus::Browser;
        self.scroll_browser_to_selection();
    }

    /// `⌘⌥B`: shows and focuses a hidden panel, hides a shown one.
    pub(super) fn toggle_browser(&mut self) {
        if self.browser.visible {
            self.browser.visible = false;
            if self.key_focus == KeyFocus::Browser {
                self.key_focus = KeyFocus::Pane;
            }
            self.browser.tree.delete_armed = false;
        } else {
            self.show_browser();
        }
    }

    /// Keeps the selected row on screen after a keyboard move.
    fn scroll_browser_to_selection(&mut self) {
        if let Some(selected) = self.browser.tree.selected_index() {
            self.browser.scroll =
                scroll_to_show(self.browser.scroll, selected, self.browser_visible_rows());
        }
    }

    /// Scrolls the list by a wheel / two-finger delta (points, positive =
    /// content moves down), clamped to the rows.
    pub(super) fn scroll_browser(&mut self, delta_y: f32) {
        let rows = self.browser.tree.rows().len();
        let max = rows.saturating_sub(self.browser_visible_rows());
        let steps = (-delta_y / BROWSER_ROW_H).round() as i64;
        self.browser.scroll = (self.browser.scroll as i64 + steps).clamp(0, max as i64) as usize;
    }

    /// A key press while the panel has the keyboard. Returns `true` when the
    /// panel consumed it; everything else falls through to the view
    /// underneath. Every arrow is consumed, whatever its modifiers, so the
    /// arrows never act on two panes at once.
    pub(super) fn handle_browser_key(&mut self, event: &InputEvent) -> bool {
        let InputEvent::KeyPressed { key, .. } = event else {
            return false;
        };
        let tree = &mut self.browser.tree;
        match key {
            Key::ArrowUp => tree.move_selection(-1),
            Key::ArrowDown => tree.move_selection(1),
            Key::ArrowRight => tree.expand(),
            Key::ArrowLeft => tree.collapse(),
            Key::Delete | Key::Backspace => tree.arm_delete(),
            Key::Enter => {
                if let Some(action) = tree.activate() {
                    self.run_browser_action(action);
                }
            }
            Key::Escape => {
                // A row dragged out of the panel is dropped nowhere first.
                let midi = self.gesture.midi_drag.take();
                let plugin = self.gesture.plugin_drag.take();
                if midi.is_some() || plugin.is_some() {
                    self.browser.press = None;
                } else if !std::mem::take(&mut tree.delete_armed) {
                    self.key_focus = KeyFocus::Pane;
                }
            }
            _ => return false,
        }
        self.scroll_browser_to_selection();
        true
    }

    /// A press at canvas `(x, y)` inside the panel (`x < 0`): focuses it,
    /// selects the row under the pointer, and toggles a folder or category
    /// pressed on its disclosure triangle. `double` is the double-click that
    /// follows a double-click's second press (already handled as a press): it
    /// toggles a folder or category and acts like Enter on anything else.
    pub(super) fn handle_browser_click(&mut self, x: f32, y: f32, double: bool) {
        self.key_focus = KeyFocus::Browser;
        if y < BROWSER_LIST_TOP {
            return;
        }
        let idx = self.browser.scroll + ((y - BROWSER_LIST_TOP) / BROWSER_ROW_H) as usize;
        let Some(item) = self
            .browser
            .tree
            .rows()
            .get(idx)
            .map(|row| row.item.clone())
        else {
            return;
        };
        let depth = self.browser.tree.rows()[idx].depth;
        let disclosure_x = f32::from(depth) * BROWSER_INDENT_X;
        let on_disclosure =
            (disclosure_x..disclosure_x + BROWSER_DISCLOSURE_W).contains(&(x + BROWSER_W));
        if !double && matches!(item, BrowserItem::MidiFile { .. } | BrowserItem::Plugin(_)) {
            self.browser.press = Some(BrowserPress {
                item: item.clone(),
                origin: (x, y),
                dragging: false,
            });
        }
        let tree = &mut self.browser.tree;
        tree.select(item.clone());
        match item {
            BrowserItem::Category(_) | BrowserItem::Folder(_) => {
                if double || on_disclosure {
                    tree.toggle(&item);
                }
            }
            _ if double => {
                if let Some(action) = tree.activate() {
                    self.run_browser_action(action);
                }
            }
            _ => {}
        }
    }

    /// Off macOS there is no plugin host, and no Plugins category to pick
    /// from.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn put_plugin_on_track(&mut self, _track_idx: usize, _plugin: &BrowserPlugin) {}

    /// Carries out what Enter asked for.
    fn run_browser_action(&mut self, action: BrowserAction) {
        match action {
            BrowserAction::OpenProject { folder, name } => {
                // Its folder becomes current once it has loaded
                // (`ProjectLoaded`): until then — through an unsaved-changes
                // prompt's Save — the open project's folder still is.
                self.request_project_action(ProjectAction::Open {
                    filename: name,
                    folder,
                });
            }
            BrowserAction::SetCurrentFolder(folder) => {
                self.set_current_project_folder(Some(folder));
            }
            BrowserAction::LoadPlugin(plugin) => {
                self.put_plugin_on_track(self.selected_track_idx, &plugin);
            }
            BrowserAction::ImportMidi { folder, name } => {
                self.import_midi_file(&midi_file_path(folder.as_deref(), &name));
            }
            BrowserAction::DeleteProject { folder, name } => {
                let message = match delete_project(folder.as_deref(), &name) {
                    Ok(()) => format!("Deleted {name}"),
                    Err(e) => format!("Could not delete {name}: {e}"),
                };
                self.render.status = Some(StatusMessage::new(message));
                self.reload_browser();
            }
        }
    }

    /// Makes `folder` the current project folder (`None`: the projects root)
    /// — where ⌘/Ctrl+N/S put a new project — persisting it to
    /// `settings.json` so it's remembered across sessions. A no-op if it
    /// already is.
    pub(super) fn set_current_project_folder(&mut self, folder: Option<String>) {
        if self.project.project_current_folder == folder {
            return;
        }
        self.project.project_current_folder = folder.clone();
        update_settings("last project folder", |settings| {
            settings.last_project_folder = folder;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(projects: &[&str], midi_files: &[&str]) -> FolderListing {
        FolderListing {
            projects: projects.iter().map(|s| s.to_string()).collect(),
            midi_files: midi_files.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Two folders — `a` with two projects and a MIDI file, empty `b` — and a
    /// loose project at the root.
    fn tree() -> BrowserTree {
        let mut tree = BrowserTree::default();
        tree.reload(
            vec![
                ("a".into(), listing(&["a2", "a1"], &["a1_T1_B1"])),
                ("b".into(), listing(&[], &[])),
            ],
            listing(&["loose"], &[]),
        );
        tree
    }

    fn folder(name: &str) -> BrowserItem {
        BrowserItem::Folder(name.into())
    }

    fn project(folder: Option<&str>, name: &str) -> BrowserItem {
        BrowserItem::Project {
            folder: folder.map(str::to_owned),
            name: name.into(),
        }
    }

    fn items(tree: &BrowserTree) -> Vec<BrowserItem> {
        tree.rows().iter().map(|row| row.item.clone()).collect()
    }

    fn projects() -> BrowserItem {
        BrowserItem::Category(BrowserCategory::Projects)
    }

    fn plugins() -> BrowserItem {
        BrowserItem::Category(BrowserCategory::Plugins)
    }

    fn plugin(name: &str, format: &'static str) -> BrowserPlugin {
        BrowserPlugin {
            name: name.into(),
            format,
            bundle_path: PathBuf::from(format!("/{name}.{format}")),
            plugin_id: name.into(),
        }
    }

    /// [`tree`] with a Plugins category listing `plugins`.
    fn tree_with_plugins(plugins: Vec<BrowserPlugin>, scanning: bool) -> BrowserTree {
        let mut tree = tree();
        tree.set_plugins(Some(PluginListing::new(plugins, scanning)));
        tree
    }

    #[test]
    fn collapsed_folders_list_alone_then_the_loose_files() {
        assert_eq!(
            items(&tree()),
            vec![projects(), folder("a"), folder("b"), project(None, "loose")]
        );
    }

    #[test]
    fn without_a_plugin_host_there_is_no_plugins_category() {
        assert!(!items(&tree()).contains(&plugins()));
    }

    #[test]
    fn plugins_list_in_catalog_order_then_scanning() {
        let tree = tree_with_plugins(
            vec![
                plugin("Analog", "VST3"),
                plugin("Diva", "CLAP"),
                plugin("diva", "VST3"),
            ],
            true,
        );
        let rows = tree.rows();
        let plugin_rows: Vec<_> = rows
            .iter()
            .skip_while(|row| row.item != plugins())
            .collect();
        assert_eq!(
            plugin_rows
                .iter()
                .map(|row| row.item.clone())
                .collect::<Vec<_>>(),
            vec![
                plugins(),
                BrowserItem::Plugin(plugin("Analog", "VST3")),
                BrowserItem::Plugin(plugin("Diva", "CLAP")),
                BrowserItem::Plugin(plugin("diva", "VST3")),
                BrowserItem::Scanning,
            ]
        );
        assert_eq!(plugin_rows[0].depth, 0);
        assert!(plugin_rows[1..].iter().all(|row| row.depth == 1));

        let done = tree_with_plugins(vec![plugin("Analog", "VST3")], false);
        assert!(!items(&done).contains(&BrowserItem::Scanning));
    }

    #[test]
    fn enter_on_a_plugin_loads_it() {
        let mut tree = tree_with_plugins(vec![plugin("Diva", "CLAP")], true);
        tree.select(BrowserItem::Plugin(plugin("Diva", "CLAP")));
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::LoadPlugin(plugin("Diva", "CLAP")))
        );
        tree.select(BrowserItem::Scanning);
        assert_eq!(tree.activate(), None);

        tree.select(BrowserItem::Plugin(plugin("Diva", "CLAP")));
        tree.arm_delete();
        assert!(!tree.delete_armed, "plugins are never deleted");
    }

    #[test]
    fn a_category_collapses_and_left_steps_out_to_it() {
        let mut tree = tree_with_plugins(vec![plugin("Diva", "CLAP")], false);
        tree.select(BrowserItem::Plugin(plugin("Diva", "CLAP")));
        tree.collapse();
        assert_eq!(tree.selected(), Some(&plugins()));
        tree.collapse();
        assert_eq!(items(&tree).last(), Some(&plugins()));

        // Enter toggles a category; Right re-expands then steps in.
        assert_eq!(tree.activate(), None);
        assert!(tree.is_category_expanded(BrowserCategory::Plugins));
        tree.collapse();
        tree.expand();
        tree.expand();
        assert_eq!(
            tree.selected(),
            Some(&BrowserItem::Plugin(plugin("Diva", "CLAP")))
        );

        // A collapsed folder steps out to Projects; collapsing Projects hides
        // every folder.
        tree.select(folder("a"));
        tree.collapse();
        assert_eq!(tree.selected(), Some(&projects()));
        tree.collapse();
        assert!(!items(&tree).contains(&folder("a")));
    }

    #[test]
    fn a_growing_catalog_keeps_the_selection() {
        let mut tree = tree_with_plugins(vec![plugin("Diva", "CLAP")], true);
        tree.select(BrowserItem::Plugin(plugin("Diva", "CLAP")));
        tree.set_plugins(Some(PluginListing::new(
            vec![plugin("Diva", "CLAP"), plugin("Analog", "VST3")],
            false,
        )));
        assert_eq!(
            tree.selected(),
            Some(&BrowserItem::Plugin(plugin("Diva", "CLAP")))
        );
    }

    #[test]
    fn an_expanded_folder_lists_projects_then_midi_files_indented() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        let rows = tree.rows();
        assert_eq!(
            rows.iter().map(|r| r.item.clone()).collect::<Vec<_>>(),
            vec![
                projects(),
                folder("a"),
                project(Some("a"), "a2"),
                project(Some("a"), "a1"),
                BrowserItem::MidiFile {
                    folder: Some("a".into()),
                    name: "a1_T1_B1".into()
                },
                folder("b"),
                project(None, "loose"),
            ]
        );
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[6].depth, 1);
    }

    #[test]
    fn right_expands_then_steps_in_and_left_steps_out_then_collapses() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        assert!(tree.is_expanded("a"));
        assert_eq!(tree.selected(), Some(&folder("a")));
        tree.expand();
        assert_eq!(tree.selected(), Some(&project(Some("a"), "a2")));
        tree.collapse();
        assert_eq!(tree.selected(), Some(&folder("a")));
        tree.collapse();
        assert!(!tree.is_expanded("a"));
    }

    #[test]
    fn selection_moves_clamped_and_starts_at_the_top() {
        let mut tree = tree();
        tree.move_selection(1);
        assert_eq!(tree.selected(), Some(&projects()));
        tree.move_selection(10);
        assert_eq!(tree.selected(), Some(&project(None, "loose")));
        tree.move_selection(-10);
        assert_eq!(tree.selected(), Some(&projects()));
    }

    #[test]
    fn enter_opens_a_project_and_makes_a_folder_current() {
        let mut tree = tree();
        tree.select(project(None, "loose"));
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::OpenProject {
                folder: None,
                name: "loose".into()
            })
        );
        tree.select(folder("b"));
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::SetCurrentFolder("b".into()))
        );
        assert!(tree.is_expanded("b"));
    }

    #[test]
    fn enter_imports_a_midi_file_which_is_never_deleted() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        tree.select(BrowserItem::MidiFile {
            folder: Some("a".into()),
            name: "a1_T1_B1".into(),
        });
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::ImportMidi {
                folder: Some("a".into()),
                name: "a1_T1_B1".into()
            })
        );
        tree.arm_delete();
        assert!(!tree.delete_armed);
    }

    #[test]
    fn delete_needs_a_second_enter_and_moving_cancels_it() {
        let mut tree = tree();
        tree.select(project(None, "loose"));
        tree.arm_delete();
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::DeleteProject {
                folder: None,
                name: "loose".into()
            })
        );

        tree.arm_delete();
        tree.move_selection(-1);
        assert!(!tree.delete_armed);

        tree.select(folder("a"));
        tree.arm_delete();
        assert!(!tree.delete_armed, "folders are never deleted");
    }

    #[test]
    fn reload_keeps_expansion_and_hands_a_deleted_selection_to_the_next_row() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        tree.select(project(Some("a"), "a2"));
        tree.reload(
            vec![
                ("a".into(), listing(&["a1"], &["a1_T1_B1"])),
                ("b".into(), listing(&[], &[])),
            ],
            listing(&["loose"], &[]),
        );
        assert!(tree.is_expanded("a"));
        assert_eq!(tree.selected(), Some(&project(Some("a"), "a1")));
    }

    #[test]
    fn reload_forgets_a_vanished_folders_expansion() {
        let mut tree = tree();
        tree.select(folder("b"));
        tree.expand();
        tree.reload(
            vec![("a".into(), listing(&[], &[]))],
            FolderListing::default(),
        );
        assert!(!tree.is_expanded("b"));
        assert_eq!(tree.selected(), Some(&folder("a")));
    }

    #[test]
    fn initial_selection_is_the_open_project_in_its_expanded_folder() {
        let mut tree = tree();
        tree.select_initial(Some("a"), Some("a1"));
        assert!(tree.is_expanded("a"));
        assert_eq!(tree.selected(), Some(&project(Some("a"), "a1")));
    }

    #[test]
    fn initial_selection_falls_back_to_the_folder_then_the_top() {
        let mut stale = tree();
        stale.select_initial(Some("b"), Some("gone"));
        assert_eq!(stale.selected(), Some(&folder("b")));

        let mut fresh = tree();
        fresh.select_initial(None, None);
        assert_eq!(fresh.selected(), Some(&projects()));

        // Only the first show picks; later shows keep the user's selection.
        fresh.select(folder("b"));
        fresh.select_initial(Some("a"), None);
        assert_eq!(fresh.selected(), Some(&folder("b")));
    }

    #[test]
    fn scroll_moves_only_as_far_as_needed() {
        assert_eq!(scroll_to_show(0, 3, 10), 0);
        assert_eq!(scroll_to_show(0, 12, 10), 3);
        assert_eq!(scroll_to_show(5, 2, 10), 2);
        assert_eq!(scroll_to_show(5, 14, 10), 5);
    }
}
