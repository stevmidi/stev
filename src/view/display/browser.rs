//! The browser side panel: a toggleable panel on the left, its width dragged
//! by its right edge, with four categories. **Projects** holds the project
//! folders under `paths::project_dir()`, each expandable to its `.stev`
//! files, then the loose projects at the projects root. **Templates** lists
//! the `.stev` files in `paths::templates_dir()`: Enter opens one as a new,
//! untitled project (`340-templates.md`). **Clips** lists the
//! `.mid` files in the `Clips` folder, where a clip export lands: Enter or a
//! drag imports one. **Plugins** (macOS only, where the plugin host runs)
//! lists every installed instrument plugin: Enter puts the selected one on
//! the selected track, a drag onto a track puts it there. The panel replaced
//! the ⌘O Project modal. `⌘⌥B` toggles it, `⌘O` shows it.
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

use std::{collections::HashSet, path::PathBuf, time::Instant};

use egui::{Key, Pos2, Vec2, vec2};

use crate::core::{
    config::BROWSER_W_DEFAULT,
    input_event::is_pane_focus_key,
    keymap::Action,
    project::{
        ProjectAction, clip_path, delete_clip, delete_project, delete_project_folder, list_clips,
        list_project_folders, list_projects, list_templates, template_path,
    },
    settings::update_settings,
};

use super::browser_rename::BrowserRename;
use super::state::scrolled_offset;
use super::*;

/// The narrowest the panel's edge drags it.
const BROWSER_W_MIN: f32 = 180.0;
/// The widest the panel's edge drags it.
const BROWSER_W_MAX: f32 = 600.0;
/// How far either side of the panel's right edge a press grabs it.
const BROWSER_EDGE_GRAB_X: f32 = 4.0;
/// Height of one row of the tree.
pub(super) const BROWSER_ROW_H: f32 = 26.0;
/// Size of the tree's text, and of the rename field over a row: what Zed's
/// project panel draws at its default 16pt UI size (its labels are 14/16 of
/// it), a point above `theme::FONT_SIZE_LABEL`.
pub(super) const BROWSER_FONT_SIZE: f32 = 14.0;
/// The list's gap below the window's top (where its first row starts) and
/// above the footer: a row the list cuts off is clipped this far short of
/// the footer's edge.
pub(super) const BROWSER_LIST_PAD_Y: f32 = 6.0;
/// Width of the disclosure triangle's hit zone, from a folder or category
/// row's indent — a click there toggles it rather than only selecting it.
pub(super) const BROWSER_DISCLOSURE_W: f32 = 26.0;
/// Indent per tree depth.
pub(super) const BROWSER_INDENT_X: f32 = 14.0;

/// `width` as the panel's width: whole points, so the shifted canvas keeps
/// its pixel-snapped seams, between [`BROWSER_W_MIN`] and [`BROWSER_W_MAX`].
/// A width read from `settings.json` passes through here too.
pub(in crate::view::display) fn clamp_browser_width(width: f32) -> f32 {
    if width.is_finite() {
        width.round().clamp(BROWSER_W_MIN, BROWSER_W_MAX)
    } else {
        BROWSER_W_DEFAULT
    }
}

/// Row `idx` of the tree with the list scrolled `scroll` points, in a panel
/// `width` wide whose top-left corner is `origin`: the panel's width less its
/// separator. Where the rows are drawn and the rename field sits.
pub(super) fn browser_row_rect(origin: Pos2, width: f32, idx: usize, scroll: f32) -> Rect {
    let top = origin.y + BROWSER_LIST_PAD_Y + idx as f32 * BROWSER_ROW_H - scroll;
    Rect::from_min_max(
        pos2(origin.x, top),
        pos2(origin.x + width - 1.0, top + BROWSER_ROW_H),
    )
}

/// The row index under `y`, measured from the panel's top, with the list
/// scrolled `scroll` points — the inverse of [`browser_row_rect`]. `None`
/// in the pad above the first row; past the last row it is an index the
/// tree doesn't have.
pub(super) fn browser_row_at(y: f32, scroll: f32) -> Option<usize> {
    (y >= BROWSER_LIST_PAD_Y).then(|| ((y - BROWSER_LIST_PAD_Y + scroll) / BROWSER_ROW_H) as usize)
}

/// The footer band in canvas space, given the `canvas` rect, the footer's
/// top and the panel's width this frame: from the window's left edge (canvas
/// x `-browser_w`, the canvas being drawn shifted right by the panel) to the
/// canvas's right, so it spans the whole window under the panel and doesn't
/// move when the panel shows or hides.
pub(super) fn footer_rect(canvas: Rect, footer_top: f32, browser_w: f32) -> Rect {
    Rect::from_min_max(pos2(canvas.min.x - browser_w, footer_top), canvas.max)
}

/// The footer's panel toggle: a `STATUS_H` square at the footer's left
/// edge, `BROWSER_TOGGLE_INSET_X` in and centred on the whole band (status
/// bar and inset read as one panel), in the space of `footer` (see
/// [`footer_rect`]) — so it sits at the window's left edge either way and a
/// second click lands on it again.
pub(super) fn browser_toggle_rect(footer: Rect) -> Rect {
    let side = theme::STATUS_H;
    Rect::from_center_size(
        pos2(
            footer.min.x + BROWSER_TOGGLE_INSET_X + side * 0.5,
            footer.center().y,
        ),
        vec2(side, side),
    )
}

/// Side of the rounded square washed behind the panel toggle's icon under
/// the pointer, and of a category row's `+` (its wash and hit area).
pub(super) const TOGGLE_WASH_SIZE: f32 = 22.0;
/// Right inset of a category row's `+` from the panel's edge.
const CATEGORY_PLUS_RIGHT_X: f32 = 6.0;

/// The `+` on the category row `row`: a square at the row's right end,
/// centred on it.
pub(super) fn category_plus_rect(row: Rect) -> Rect {
    Rect::from_center_size(
        pos2(
            row.max.x - CATEGORY_PLUS_RIGHT_X - TOGGLE_WASH_SIZE * 0.5,
            row.center().y,
        ),
        Vec2::splat(TOGGLE_WASH_SIZE),
    )
}

/// The footer's word for the project, clip or folder `name` deleted: moved to
/// the Trash on macOS; elsewhere a file is removed for good and a folder only
/// when empty (`delete_project_folder`).
fn deleted_message(name: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("Moved {name} to the Trash")
    } else {
        format!("Deleted {name}")
    }
}

/// The panel toggle's gap from the window's left edge.
const BROWSER_TOGGLE_INSET_X: f32 = 4.0;

/// Width of the icon column of a row below a category: the icon and its gap
/// to the text.
pub(super) const BROWSER_ICON_W: f32 = 22.0;

/// Where the icon column of a row at `depth` starts, from the row's left
/// edge: past its indent and the disclosure triangle's zone.
pub(super) fn browser_icon_x(depth: u8) -> f32 {
    f32::from(depth) * BROWSER_INDENT_X + BROWSER_DISCLOSURE_W
}

/// Where the text of `row` starts, from the row's left edge: past the icon
/// column, which every row but a category has — iconless "Scanning…" too, so
/// the names line up.
pub(super) fn browser_text_x(row: &BrowserRow) -> f32 {
    let icon = if matches!(row.item, BrowserItem::Category(_)) {
        0.0
    } else {
        BROWSER_ICON_W
    };
    browser_icon_x(row.depth) + icon
}

/// A top-level category of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum BrowserCategory {
    /// The project folders and their projects.
    Projects,
    /// The project templates.
    Templates,
    /// The MIDI clips in the `Clips` folder.
    Clips,
    /// The installed instrument plugins.
    Plugins,
}

impl BrowserCategory {
    /// The command the `+` at the hovered row's right end runs, for a row
    /// that has one: Projects' makes a new folder, Templates' saves the open
    /// project as a template. The one place a `+` is given its work — the
    /// press and the footer hint both read it.
    pub(super) fn plus_action(self) -> Option<Action> {
        match self {
            BrowserCategory::Projects => Some(Action::NewProjectFolder),
            BrowserCategory::Templates => Some(Action::SaveAsTemplate),
            BrowserCategory::Clips | BrowserCategory::Plugins => None,
        }
    }

    /// The category row's label.
    pub(super) fn label(self) -> &'static str {
        match self {
            BrowserCategory::Projects => "PROJECTS",
            BrowserCategory::Templates => "TEMPLATES",
            BrowserCategory::Clips => "CLIPS",
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
    /// A `.stev` in the templates folder, by its file stem. Enter opens it
    /// as a new, untitled project.
    Template(String),
    /// A `.mid` in the `Clips` folder, by its file stem. Enter imports it on
    /// the selected track at the cursor; dragged out of the panel, it imports
    /// where it is dropped (`input/midi_drag.rs`).
    Clip(String),
    /// An installed instrument plugin. Enter puts it on the selected track;
    /// dragged out of the panel, on the track it is dropped on
    /// (`input/plugin_drag.rs`).
    Plugin(BrowserPlugin),
    /// The inert "Scanning…" row ending the Plugins category while the
    /// catalog scan runs.
    Scanning,
    /// The row a new folder is named on: first under Projects while its
    /// name field is open (⌘⇧N, or the `+` on the Projects row). Nothing is
    /// on disk until Enter.
    NewFolder,
}

impl BrowserItem {
    /// The row Left steps out to: a project's folder (or the Projects
    /// category at the projects root), a folder's, a template's, a clip's or
    /// a plugin's category. A category has none.
    fn parent(&self) -> Option<BrowserItem> {
        match self {
            BrowserItem::Category(_) => None,
            BrowserItem::Project { folder, .. } => Some(folder.clone().map_or(
                BrowserItem::Category(BrowserCategory::Projects),
                BrowserItem::Folder,
            )),
            BrowserItem::Folder(_) | BrowserItem::NewFolder => {
                Some(BrowserItem::Category(BrowserCategory::Projects))
            }
            BrowserItem::Template(_) => Some(BrowserItem::Category(BrowserCategory::Templates)),
            BrowserItem::Clip(_) => Some(BrowserItem::Category(BrowserCategory::Clips)),
            BrowserItem::Plugin(_) | BrowserItem::Scanning => {
                Some(BrowserItem::Category(BrowserCategory::Plugins))
            }
        }
    }

    /// A project's, clip's or folder's name (a file's stem) — the rows ⌘R
    /// renames; `None` for anything else.
    pub(super) fn renamable_name(&self) -> Option<&str> {
        match self {
            BrowserItem::Project { name, .. }
            | BrowserItem::Clip(name)
            | BrowserItem::Folder(name) => Some(name),
            _ => None,
        }
    }

    /// The same project, clip or folder under `new_name` — what a rename
    /// leaves behind — or the folder the new folder's row made; `None` for a
    /// row ⌘R doesn't rename.
    pub(super) fn renamed(&self, new_name: String) -> Option<BrowserItem> {
        if *self == BrowserItem::NewFolder {
            return Some(BrowserItem::Folder(new_name));
        }
        let mut item = self.clone();
        let (BrowserItem::Project { name, .. }
        | BrowserItem::Clip(name)
        | BrowserItem::Folder(name)) = &mut item
        else {
            return None;
        };
        *name = new_name;
        Some(item)
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
    /// Start a new project from this template (its file stem).
    OpenTemplate(String),
    /// Put this plugin on the selected track.
    LoadPlugin(BrowserPlugin),
    /// Import this clip (its file stem) on the selected track at the cursor.
    ImportClip(String),
    /// Delete this project (the confirmed second Enter).
    DeleteProject {
        /// Its folder, `None` at the projects root.
        folder: Option<String>,
        /// The project name.
        name: String,
    },
    /// Delete this project folder, with its projects (the confirmed second
    /// Enter).
    DeleteFolder(String),
    /// Delete this clip, by its file stem (the confirmed second Enter).
    DeleteClip(String),
}

/// What [`BrowserTree::reload`] lists from disk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DiskListing {
    /// Each project folder with its projects (newest first), alphabetical.
    pub(super) folders: Vec<(String, Vec<String>)>,
    /// The loose projects at the projects root, newest first.
    pub(super) root: Vec<String>,
    /// The templates in the templates folder, alphabetical.
    pub(super) templates: Vec<String>,
    /// The clips in the `Clips` folder, alphabetical.
    pub(super) clips: Vec<String>,
}

impl DiskListing {
    /// The projects in the folder `name`; `None` when there is no such
    /// folder.
    fn folder_projects(&self, name: &str) -> Option<&[String]> {
        self.folders
            .iter()
            .find(|(folder, _)| folder == name)
            .map(|(_, projects)| projects.as_slice())
    }
}

/// The tree: what is on disk (refreshed whenever the panel is shown and
/// after a delete), the plugins (refreshed as the catalog scan reports),
/// which categories and folders are expanded and which row is selected.
/// Expansion and selection survive a refresh and hiding the panel; neither is
/// persisted. Every category starts expanded, every folder collapsed.
#[derive(Default)]
pub(super) struct BrowserTree {
    /// The projects and clips.
    disk: DiskListing,
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
    /// When the selected project, clip or folder was armed to be deleted, while
    /// it awaits its confirm (the second Enter): the footer shows the newer
    /// of its question and a passing message. Any selection change disarms
    /// it.
    pub(super) delete_armed: Option<Instant>,
    /// Whether the [`NewFolder`](BrowserItem::NewFolder) row is listed: while
    /// its name field is open. Set and cleared with the field only
    /// (`open_new_folder_field`, `close_browser_rename`), so the two never
    /// disagree.
    new_folder: bool,
}

/// Appends `projects`, in `folder` (`None`: the projects root), as rows at
/// `depth`.
fn push_project_rows(
    rows: &mut Vec<BrowserRow>,
    folder: Option<&str>,
    projects: &[String],
    depth: u8,
) {
    let folder = folder.map(str::to_owned);
    rows.extend(projects.iter().map(|name| BrowserRow {
        item: BrowserItem::Project {
            folder: folder.clone(),
            name: name.clone(),
        },
        depth,
    }));
}

impl BrowserTree {
    /// Replaces what the tree lists, keeping expansion and selection. A
    /// selected item that is gone hands the selection to the row now at its
    /// old index (clamped), so a delete lands on the next row.
    pub(super) fn reload(&mut self, disk: DiskListing) {
        let old_index = self.selected_index();
        self.disk = disk;
        self.expanded
            .retain(|name| self.disk.folder_projects(name).is_some());
        self.delete_armed = None;
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
    /// the new folder's row while it is named, then each folder, followed (when expanded) by its projects newest first;
    /// then the projects root's loose projects. Under **Templates** (when
    /// expanded): the templates. Under **Clips** (when
    /// expanded): the clips. Under **Plugins** (when listed and expanded):
    /// the plugins, then "Scanning…" while the scan runs.
    fn rebuild_rows(&mut self) {
        let mut rows = vec![BrowserRow {
            item: BrowserItem::Category(BrowserCategory::Projects),
            depth: 0,
        }];
        if self.is_category_expanded(BrowserCategory::Projects) {
            if self.new_folder {
                rows.push(BrowserRow {
                    item: BrowserItem::NewFolder,
                    depth: 1,
                });
            }
            for (name, projects) in &self.disk.folders {
                rows.push(BrowserRow {
                    item: BrowserItem::Folder(name.clone()),
                    depth: 1,
                });
                if self.expanded.contains(name) {
                    push_project_rows(&mut rows, Some(name), projects, 2);
                }
            }
            push_project_rows(&mut rows, None, &self.disk.root, 1);
        }
        self.push_flat_category(
            &mut rows,
            BrowserCategory::Templates,
            &self.disk.templates,
            BrowserItem::Template,
        );
        self.push_flat_category(
            &mut rows,
            BrowserCategory::Clips,
            &self.disk.clips,
            BrowserItem::Clip,
        );
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

    /// Appends `category`'s row and, when it is expanded, one row per name
    /// under it, made by `item`.
    fn push_flat_category(
        &self,
        rows: &mut Vec<BrowserRow>,
        category: BrowserCategory,
        names: &[String],
        item: fn(String) -> BrowserItem,
    ) {
        rows.push(BrowserRow {
            item: BrowserItem::Category(category),
            depth: 0,
        });
        if self.is_category_expanded(category) {
            rows.extend(names.iter().map(|name| BrowserRow {
                item: item(name.clone()),
                depth: 1,
            }));
        }
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
        self.index_of(self.selected.as_ref()?)
    }

    /// `item`'s index in [`rows`](Self::rows), when it is listed.
    pub(super) fn index_of(&self, item: &BrowserItem) -> Option<usize> {
        self.rows.iter().position(|row| &row.item == item)
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

    /// Lists the [`NewFolder`](BrowserItem::NewFolder) row first under
    /// Projects, expanding the category, or takes it away. The selection
    /// stays where it is.
    pub(super) fn set_new_folder(&mut self, shown: bool) {
        if shown {
            self.set_item_expanded(&BrowserItem::Category(BrowserCategory::Projects), true);
        }
        if self.new_folder != shown {
            self.new_folder = shown;
            self.rebuild_rows();
        }
    }

    /// Keeps a renamed folder's expansion under its new name — before the
    /// [`reload`](Self::reload) that lists it, which forgets the old one.
    pub(super) fn carry_folder_expansion(&mut self, old: &str, new: &str) {
        if self.expanded.remove(old) {
            self.expanded.insert(new.to_owned());
        }
    }

    /// What an armed delete asks of the selected row, in the footer (its
    /// width no matter how narrow the panel): a folder says how many
    /// projects go with it. `None` with no delete armed.
    pub(super) fn delete_prompt(&self) -> Option<String> {
        self.delete_armed?;
        let what = match self.selected.as_ref()? {
            BrowserItem::Project { name, .. } | BrowserItem::Clip(name) => name.clone(),
            BrowserItem::Folder(name) => {
                let count = self.disk.folder_projects(name).map_or(0, <[_]>::len);
                match count {
                    0 => name.clone(),
                    1 => format!("{name} and its project"),
                    n => format!("{name} and its {n} projects"),
                }
            }
            _ => return None,
        };
        Some(format!("Delete {what}? Enter deletes, Esc cancels"))
    }

    /// Selects `item` and disarms a pending delete.
    pub(super) fn select(&mut self, item: BrowserItem) {
        self.selected = Some(item);
        self.delete_armed = None;
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
            && self.disk.folder_projects(folder).is_some()
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

    /// Enter: confirms a pending delete (a project, clip or folder); else opens the selected project
    /// or template, expands/collapses the selected folder or category, imports the
    /// selected clip or puts the selected plugin on the selected track.
    /// A folder is never made current here: Save As picks where a project
    /// goes, defaulting to the selection (`save_folder_default`).
    pub(super) fn activate(&mut self) -> Option<BrowserAction> {
        let armed = self.delete_armed.take().is_some();
        let item = self.selected.clone()?;
        match item {
            BrowserItem::Project { folder, name } if armed => {
                Some(BrowserAction::DeleteProject { folder, name })
            }
            BrowserItem::Folder(name) if armed => Some(BrowserAction::DeleteFolder(name)),
            BrowserItem::Clip(name) if armed => Some(BrowserAction::DeleteClip(name)),
            BrowserItem::Category(_) | BrowserItem::Folder(_) => {
                self.toggle(&item);
                None
            }
            BrowserItem::Plugin(plugin) => Some(BrowserAction::LoadPlugin(plugin)),
            BrowserItem::Scanning | BrowserItem::NewFolder => None,
            BrowserItem::Project { folder, name } => {
                Some(BrowserAction::OpenProject { folder, name })
            }
            BrowserItem::Template(name) => Some(BrowserAction::OpenTemplate(name)),
            BrowserItem::Clip(name) => Some(BrowserAction::ImportClip(name)),
        }
    }

    /// Delete/Backspace: arms the delete confirm on a selected project, clip
    /// or folder.
    pub(super) fn arm_delete(&mut self) {
        self.delete_armed = matches!(
            self.selected,
            Some(BrowserItem::Project { .. } | BrowserItem::Clip(_) | BrowserItem::Folder(_))
        )
        .then(Instant::now);
    }
}

/// The browser list's scroll (points) after scrolling so row `selected` is
/// wholly on screen, moving `scroll` as little as possible; the list is
/// `list_h` points tall. A list shorter than a row shows the row's top.
pub(super) fn browser_scroll_to_show(scroll: f32, selected: usize, list_h: f32) -> f32 {
    let top = selected as f32 * BROWSER_ROW_H;
    if top < scroll {
        top
    } else if top + BROWSER_ROW_H > scroll + list_h {
        (top + BROWSER_ROW_H - list_h).min(top)
    } else {
        scroll
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
    /// The width while shown, in points ([`clamp_browser_width`]), kept in
    /// `settings.json` once its edge is dragged.
    pub(super) width: f32,
    /// While the right edge is dragged: the pointer's offset from the edge
    /// at the press, so the edge doesn't jump to the pointer.
    pub(super) resize_grab: Option<f32>,
    /// The tree.
    pub(super) tree: BrowserTree,
    /// How far the list is scrolled, in points: a float, so trackpad deltas
    /// move it smoothly and small ones add up rather than round away.
    pub(super) scroll: f32,
    /// A draggable row the primary button went down on — a Clips or a
    /// Plugins row — `Some` until the release: dragged out of the panel, it
    /// becomes the import drag (`GestureState::midi_drag`) or the plugin
    /// drag (`GestureState::plugin_drag`).
    pub(super) press: Option<BrowserPress>,
    /// The open rename field (⌘R on a project or clip row), see
    /// `browser_rename.rs`.
    pub(super) rename: Option<BrowserRename>,
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
    /// The panel's width this frame: its [`width`](BrowserPanel::width)
    /// while shown, else `0`.
    pub(super) fn browser_width(&self) -> f32 {
        if self.browser.visible {
            self.browser.width
        } else {
            0.0
        }
    }

    /// Whether canvas-space `(x, y)` is on the shown panel's right edge (at
    /// canvas x `0`), where a press drags its width.
    pub(super) fn is_on_browser_edge(&self, x: f32, y: f32) -> bool {
        self.browser.visible && x.abs() <= BROWSER_EDGE_GRAB_X && y < self.footer_top()
    }

    /// Whether the pointer is on the panel's edge or dragging it — the
    /// resize arrows.
    pub(super) fn is_pointer_on_browser_edge(&self) -> bool {
        self.browser.resize_grab.is_some()
            || self
                .canvas_pointer()
                .is_some_and(|p| self.is_on_browser_edge(p.x, p.y))
    }

    /// The edge drag with the pointer at window-space x `window_x` (not
    /// canvas space, which shifts as the panel's width changes under it).
    /// `resize_grab` is the press's canvas x, which is its offset from the
    /// edge.
    pub(super) fn extend_browser_resize(&mut self, window_x: f32) {
        if let Some(grab) = self.browser.resize_grab {
            self.browser.width = clamp_browser_width(window_x - grab);
        }
    }

    /// Releases the edge drag, keeping the width in `settings.json`.
    pub(super) fn finish_browser_resize(&mut self) {
        if self.browser.resize_grab.take().is_some() {
            let width = self.browser.width;
            update_settings("browser width", |settings| {
                settings.browser_width = width;
            });
        }
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
    /// it (the shift lives there only), for the view under any modal:
    /// `None` with no pointer over the window, or while a modal is up and
    /// has the pointer itself ([`modal_pointer`](Self::modal_pointer)), so
    /// nothing under it hovers.
    pub(super) fn canvas_pointer(&self) -> Option<Pos2> {
        self.modal_pointer().filter(|_| !self.modal_up())
    }

    /// The pointer in canvas space whether or not a modal is up — for the
    /// modals' own hover tests.
    pub(super) fn modal_pointer(&self) -> Option<Pos2> {
        self.input_poller.pointer_pos()
    }

    /// Whether canvas-space `(x, y)` is on the footer's panel toggle.
    pub(super) fn is_on_browser_toggle(&self, x: f32, y: f32) -> bool {
        browser_toggle_rect(self.footer_rect()).contains(pos2(x, y))
    }

    /// Whether the pointer is on the panel toggle — its hover wash and
    /// pointing hand.
    pub(super) fn is_pointer_on_browser_toggle(&self) -> bool {
        self.canvas_pointer()
            .is_some_and(|p| self.is_on_browser_toggle(p.x, p.y))
    }

    /// The row under the pointer — its hover wash — while nothing else owns
    /// the pointer: no edge drag or row drag, and not on the
    /// edge, whose resize arrows say a press drags rather than selects.
    pub(super) fn hovered_browser_row(&self) -> Option<usize> {
        let p = self.canvas_pointer()?;
        let free = self.browser.resize_grab.is_none()
            && !self
                .browser
                .press
                .as_ref()
                .is_some_and(|press| press.dragging);
        (free
            && self.is_over_browser(p.x, p.y)
            && p.y < self.browser_list_bottom()
            && !self.is_on_browser_edge(p.x, p.y))
        .then(|| browser_row_at(p.y, self.browser.scroll))
        .flatten()
    }

    /// Whether canvas-space `(x, y)` is over the panel: left of the canvas
    /// and above the footer, which runs under it.
    pub(super) fn is_over_browser(&self, x: f32, y: f32) -> bool {
        x < 0.0 && y < self.footer_top()
    }

    /// Where the list stops: `BROWSER_LIST_PAD_Y` short of the footer, so a
    /// part-scrolled last row is clipped clear of its edge.
    pub(super) fn browser_list_bottom(&self) -> f32 {
        self.footer_top() - BROWSER_LIST_PAD_Y
    }

    /// The height of the panel's list this frame, in points.
    pub(super) fn browser_list_h(&self) -> f32 {
        self.browser_list_bottom() - self.render.canvas_rect.min.y - BROWSER_LIST_PAD_Y
    }

    /// The furthest the list scrolls: its last row's bottom at the list's.
    fn browser_max_scroll(&self) -> f32 {
        self.browser.tree.rows().len() as f32 * BROWSER_ROW_H - self.browser_list_h()
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

    /// Re-reads the projects, templates and clips from disk.
    pub(super) fn reload_browser(&mut self) {
        let folders = list_project_folders()
            .into_iter()
            .map(|name| {
                let projects = list_projects(Some(&name));
                (name, projects)
            })
            .collect();
        self.browser.tree.reload(DiskListing {
            folders,
            root: list_projects(None),
            templates: list_templates(),
            clips: list_clips(),
        });
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
            self.hide_browser();
        } else {
            self.show_browser();
        }
    }

    /// `⌘O`: hides the panel when it already has the keyboard, else shows
    /// and focuses it — so a shown panel the canvas has the keyboard from
    /// is focused, not hidden.
    pub(super) fn open_or_hide_browser(&mut self) {
        if self.browser.visible && self.key_focus == KeyFocus::Browser {
            self.hide_browser();
        } else {
            self.show_browser();
        }
    }

    /// Hides the panel, handing its keyboard back to the view underneath
    /// and disarming a pending delete.
    fn hide_browser(&mut self) {
        self.browser.visible = false;
        if self.key_focus == KeyFocus::Browser {
            self.key_focus = KeyFocus::Pane;
        }
        self.browser.tree.delete_armed = None;
    }

    /// Keeps the selected row on screen after a keyboard move.
    pub(super) fn scroll_browser_to_selection(&mut self) {
        match self.browser.tree.selected_index() {
            Some(selected) => self.scroll_browser_to_row(selected),
            None => self.clamp_browser_scroll(),
        }
    }

    /// Scrolls the list as little as needed to show row `idx` whole.
    pub(super) fn scroll_browser_to_row(&mut self, idx: usize) {
        self.browser.scroll =
            browser_scroll_to_show(self.browser.scroll, idx, self.browser_list_h());
        self.clamp_browser_scroll();
    }

    /// Keeps the scroll within the rows: a collapse can leave the list
    /// scrolled past its new end.
    fn clamp_browser_scroll(&mut self) {
        self.browser.scroll = self.browser.scroll.min(self.browser_max_scroll()).max(0.0);
    }

    /// Row `item`'s index and rect, in window space (the panel isn't
    /// shifted), while the panel shows and the row is wholly in the list —
    /// where something drawn over it, unclipped, may go.
    pub(super) fn browser_visible_row(&self, item: &BrowserItem) -> Option<(usize, Rect)> {
        if !self.browser.visible {
            return None;
        }
        let idx = self.browser.tree.index_of(item)?;
        let row = browser_row_rect(
            self.render.canvas_rect.min,
            self.browser.width,
            idx,
            self.browser.scroll,
        );
        let list_top = self.render.canvas_rect.min.y + BROWSER_LIST_PAD_Y;
        (row.min.y >= list_top && row.max.y <= self.browser_list_bottom()).then_some((idx, row))
    }

    /// The `+` on `category`'s row, in window space. `None` while the row
    /// isn't wholly on screen.
    pub(super) fn browser_category_plus(&self, category: BrowserCategory) -> Option<Rect> {
        self.browser_visible_row(&BrowserItem::Category(category))
            .map(|(_, row)| category_plus_rect(row))
    }

    /// The category whose row's `+` canvas-space `(x, y)` is on, if any.
    fn browser_category_plus_at(&self, x: f32, y: f32) -> Option<BrowserCategory> {
        let category = match self
            .browser
            .tree
            .rows()
            .get(browser_row_at(y, self.browser.scroll)?)?
            .item
        {
            BrowserItem::Category(category) if category.plus_action().is_some() => category,
            _ => return None,
        };
        self.browser_category_plus(category)
            .is_some_and(|button| {
                button
                    .translate(vec2(-self.browser_width(), 0.0))
                    .contains(pos2(x, y))
            })
            .then_some(category)
    }

    /// The category whose row's `+` the pointer is on — the button shows
    /// while its row is hovered only (that row is the one under the pointer),
    /// its wash and pointing hand then.
    pub(super) fn browser_category_plus_under_pointer(&self) -> Option<BrowserCategory> {
        self.hovered_browser_row()?;
        let pointer = self.canvas_pointer()?;
        self.browser_category_plus_at(pointer.x, pointer.y)
    }

    /// Scrolls the list by a wheel / two-finger delta (points, positive =
    /// content moves down), clamped to the rows.
    pub(super) fn scroll_browser(&mut self, delta_y: f32) {
        self.browser.scroll =
            scrolled_offset(self.browser.scroll, delta_y, self.browser_max_scroll());
    }

    /// A key press while the panel has the keyboard. Returns `true` when the
    /// panel consumed it; everything else falls through to the view
    /// underneath. Every arrow is consumed, whatever its modifiers, so the
    /// arrows never act on two panes at once. Plain Tab gives the keyboard
    /// back to the focused pane — and only that: the pane focus stays where
    /// it was, so done-with-the-browser is one press.
    pub(super) fn handle_browser_key(&mut self, event: &InputEvent) -> bool {
        let InputEvent::KeyPressed { key, modifiers } = event else {
            return false;
        };
        // ⌘/Ctrl+R — rename the selected project, `.mid` or folder, as it
        // renames the selected track over the lanes. Exact chord.
        if *key == Key::R && modifiers.command && !modifiers.shift && !modifiers.alt {
            self.open_browser_rename();
            return true;
        }
        if is_pane_focus_key(*key, *modifiers) {
            // A delete armed and left behind must not fire on a later Enter.
            self.browser.tree.delete_armed = None;
            self.key_focus = KeyFocus::Pane;
            return true;
        }
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
                } else if tree.delete_armed.take().is_none() {
                    self.key_focus = KeyFocus::Pane;
                }
            }
            // Any other key goes on to the pane. One that runs a pane's
            // command (`⌘A`, `⌘D`, ...) takes the keyboard there first, so
            // a Delete after it acts on what it changed, not on the browser
            // row; a global one (Space, `⌘O`) leaves it here.
            _ => {
                if self
                    .keymap
                    .resolve_press(*key, *modifiers, self.focused_pane())
                    .is_some_and(Action::acts_on_pane)
                {
                    self.browser.tree.delete_armed = None;
                    self.key_focus = KeyFocus::Pane;
                }
                return false;
            }
        }
        self.scroll_browser_to_selection();
        true
    }

    /// A press at canvas `(x, y)` inside the panel (`x < 0`): focuses it,
    /// opens the new folder's field on the Projects row's `+` or Save as
    /// template on the Templates row's, else selects
    /// the row under the pointer, and toggles a folder or category
    /// pressed on its disclosure triangle. `double` is the double-click that
    /// follows a double-click's second press (already handled as a press): it
    /// toggles a folder or category and acts like Enter on anything else.
    pub(super) fn handle_browser_click(&mut self, x: f32, y: f32, double: bool) {
        self.key_focus = KeyFocus::Browser;
        // A category row's `+` runs its command, selecting nothing.
        if let Some(category) = self.browser_category_plus_at(x, y) {
            match category.plus_action() {
                _ if double => {}
                Some(Action::NewProjectFolder) => self.open_new_folder_field(),
                Some(Action::SaveAsTemplate) => self.open_save_template(),
                _ => {}
            }
            return;
        }
        let Some(idx) = browser_row_at(y, self.browser.scroll) else {
            return;
        };
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
        let on_disclosure = (disclosure_x..disclosure_x + BROWSER_DISCLOSURE_W)
            .contains(&(x + self.browser_width()));
        if !double && matches!(item, BrowserItem::Clip(_) | BrowserItem::Plugin(_)) {
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
            BrowserAction::OpenTemplate(name) => {
                self.request_project_action(ProjectAction::NewFromTemplate {
                    path: template_path(&name),
                });
            }
            BrowserAction::LoadPlugin(plugin) => {
                self.put_plugin_on_track(self.selected_track_idx, &plugin);
            }
            BrowserAction::ImportClip(name) => {
                self.import_midi_file(&clip_path(&name));
            }
            BrowserAction::DeleteProject { folder, name } => {
                let message = match delete_project(folder.as_deref(), &name) {
                    Ok(()) => deleted_message(&name),
                    Err(e) => format!("Could not delete {name}: {e}"),
                };
                self.finish_browser_delete(message);
            }
            BrowserAction::DeleteClip(name) => {
                let message = match delete_clip(&name) {
                    Ok(()) => deleted_message(&name),
                    Err(e) => format!("Could not delete {name}: {e}"),
                };
                self.finish_browser_delete(message);
            }
            BrowserAction::DeleteFolder(name) => {
                let message = self.delete_browser_folder(&name);
                self.finish_browser_delete(message);
            }
        }
    }

    /// After a delete: says how it went in the footer and re-reads the tree.
    fn finish_browser_delete(&mut self, message: String) {
        self.render.status = Some(StatusMessage::new(message));
        self.reload_browser();
    }

    /// Deletes the project folder `name` with its projects — to the Trash
    /// (macOS) — and returns the footer's message. A project open from it
    /// stays open, as the open project does when it is deleted, and ⌘S saves
    /// it back into the folder, made again; with none open from it, a current
    /// folder deleted hands over to the projects root.
    fn delete_browser_folder(&mut self, name: &str) -> String {
        let current = self.project.project_current_folder.as_deref() == Some(name);
        match delete_project_folder(name) {
            Ok(()) => {
                // The open project stays open, and its folder current, as
                // after deleting the open project: ⌘S writes both back.
                // With none open from it, a new project's ⌘S must not make
                // the deleted folder again.
                if current && self.project.project_current_name.is_none() {
                    self.set_current_project_folder(None);
                }
                deleted_message(name)
            }
            Err(e) => format!("Could not delete {name}: {e}"),
        }
    }

    /// Makes `folder` the current project folder (`None`: the projects root)
    /// — where a saved project lives, and Save As's fallback — persisting it
    /// to `settings.json` so it's remembered across sessions. A no-op if it
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

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn row_at_inverts_row_rect() {
        let origin = pos2(0.0, 0.0);
        for scroll in [0.0, 13.0, 2.0 * BROWSER_ROW_H] {
            for idx in 0..5 {
                let rect = browser_row_rect(origin, 200.0, idx, scroll);
                for y in [rect.min.y, rect.center().y, rect.max.y - 0.01] {
                    if y >= BROWSER_LIST_PAD_Y {
                        assert_eq!(
                            browser_row_at(y, scroll),
                            Some(idx),
                            "y {y} scroll {scroll}"
                        );
                    }
                }
            }
        }
        assert_eq!(browser_row_at(BROWSER_LIST_PAD_Y - 0.5, 0.0), None);
    }

    #[test]
    fn text_clears_the_icon_column_on_every_row_but_a_category() {
        let row = |item, depth| BrowserRow { item, depth };
        let category = row(BrowserItem::Category(BrowserCategory::Clips), 0);
        assert_eq!(browser_text_x(&category), browser_icon_x(0));
        for item in [BrowserItem::Clip("riff".to_owned()), BrowserItem::Scanning] {
            assert_eq!(
                browser_text_x(&row(item, 1)),
                browser_icon_x(1) + BROWSER_ICON_W
            );
        }
    }

    /// `folders` (name, projects), the loose projects `root` and `clips`.
    fn disk(folders: &[(&str, &[&str])], root: &[&str], clips: &[&str]) -> DiskListing {
        DiskListing {
            folders: folders
                .iter()
                .map(|(name, projects)| (name.to_string(), names(projects)))
                .collect(),
            root: names(root),
            templates: Vec::new(),
            clips: names(clips),
        }
    }

    /// Two folders — `a` with two projects, empty `b` — a loose project at
    /// the root, one template and one clip.
    fn tree() -> BrowserTree {
        let mut tree = BrowserTree::default();
        tree.reload(DiskListing {
            templates: names(&["Band"]),
            ..disk(
                &[("a", &["a2", "a1"]), ("b", &[])],
                &["loose"],
                &["a1_T1_B1"],
            )
        });
        tree
    }

    fn templates() -> BrowserItem {
        BrowserItem::Category(BrowserCategory::Templates)
    }

    fn template(name: &str) -> BrowserItem {
        BrowserItem::Template(name.into())
    }

    fn clips() -> BrowserItem {
        BrowserItem::Category(BrowserCategory::Clips)
    }

    fn clip(name: &str) -> BrowserItem {
        BrowserItem::Clip(name.into())
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
    fn collapsed_folders_list_alone_then_the_loose_projects_the_templates_and_the_clips() {
        assert_eq!(
            items(&tree()),
            vec![
                projects(),
                folder("a"),
                folder("b"),
                project(None, "loose"),
                templates(),
                template("Band"),
                clips(),
                clip("a1_T1_B1"),
            ]
        );
    }

    #[test]
    fn the_clips_category_shows_when_empty_and_left_steps_out_to_it() {
        let mut empty = BrowserTree::default();
        empty.reload(disk(&[], &[], &[]));
        assert_eq!(items(&empty), vec![projects(), templates(), clips()]);

        let mut tree = tree();
        tree.select(clip("a1_T1_B1"));
        tree.collapse();
        assert_eq!(tree.selected(), Some(&clips()));
        tree.collapse();
        assert!(!items(&tree).contains(&clip("a1_T1_B1")));
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
        assert!(tree.delete_armed.is_none(), "plugins are never deleted");
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
    fn an_expanded_folder_lists_its_projects_indented() {
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
                folder("b"),
                project(None, "loose"),
                templates(),
                template("Band"),
                clips(),
                clip("a1_T1_B1"),
            ]
        );
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[5].depth, 1);
        assert_eq!(rows[6].depth, 0);
        assert_eq!(rows[7].depth, 1);
        assert_eq!(rows[8].depth, 0);
        assert_eq!(rows[9].depth, 1);
    }

    /// Enter on a template opens it — as a new project, which the caller
    /// asks for — and a template is neither renamed nor deleted here; Left
    /// steps out to its category.
    #[test]
    fn enter_opens_a_template_and_left_steps_out_to_its_category() {
        let mut tree = tree();
        tree.select(template("Band"));
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::OpenTemplate("Band".to_owned()))
        );
        assert_eq!(template("Band").renamable_name(), None);
        tree.arm_delete();
        assert!(tree.delete_armed.is_none());
        tree.collapse();
        assert_eq!(tree.selected(), Some(&templates()));
        tree.collapse();
        assert!(!items(&tree).contains(&template("Band")));
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
        assert_eq!(tree.selected(), Some(&clip("a1_T1_B1")));
        tree.move_selection(-10);
        assert_eq!(tree.selected(), Some(&projects()));
    }

    #[test]
    fn enter_opens_a_project_and_toggles_a_folder() {
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
        assert_eq!(tree.activate(), None);
        assert!(tree.is_expanded("b"));
        assert_eq!(tree.activate(), None);
        assert!(!tree.is_expanded("b"));
    }

    #[test]
    fn enter_imports_a_clip_and_a_second_enter_deletes_it() {
        let mut tree = tree();
        tree.select(clip("a1_T1_B1"));
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::ImportClip("a1_T1_B1".into()))
        );
        tree.arm_delete();
        assert_eq!(
            tree.delete_prompt().as_deref(),
            Some("Delete a1_T1_B1? Enter deletes, Esc cancels")
        );
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::DeleteClip("a1_T1_B1".into()))
        );
        assert!(tree.delete_armed.is_none(), "the delete disarms itself");
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
        assert!(tree.delete_armed.is_none());

        tree.select(folder("a"));
        tree.arm_delete();
        assert_eq!(
            tree.activate(),
            Some(BrowserAction::DeleteFolder("a".into()))
        );

        tree.select(projects());
        tree.arm_delete();
        assert!(tree.delete_armed.is_none(), "a category is never deleted");
    }

    /// A folder's prompt says how many projects go with it.
    #[test]
    fn a_folders_delete_prompt_counts_its_projects() {
        let mut tree = BrowserTree::default();
        tree.reload(disk(
            &[("two", &["x", "y"]), ("one", &["x"]), ("none", &[])],
            &["loose"],
            &[],
        ));
        let prompt = |tree: &mut BrowserTree, item| {
            tree.select(item);
            assert_eq!(tree.delete_prompt(), None, "not armed yet");
            tree.arm_delete();
            tree.delete_prompt()
        };
        assert_eq!(
            prompt(&mut tree, folder("two")).as_deref(),
            Some("Delete two and its 2 projects? Enter deletes, Esc cancels")
        );
        assert_eq!(
            prompt(&mut tree, folder("one")).as_deref(),
            Some("Delete one and its project? Enter deletes, Esc cancels")
        );
        assert_eq!(
            prompt(&mut tree, folder("none")).as_deref(),
            Some("Delete none? Enter deletes, Esc cancels")
        );
        assert_eq!(
            prompt(&mut tree, project(None, "loose")).as_deref(),
            Some("Delete loose? Enter deletes, Esc cancels")
        );
    }

    /// The new folder's row comes first under Projects — expanding a
    /// collapsed category — without taking the selection, and goes again.
    #[test]
    fn the_new_folder_row_comes_first_under_projects_and_goes_again() {
        let mut tree = tree();
        tree.select(project(None, "loose"));
        tree.collapse();
        tree.collapse();
        assert_eq!(
            items(&tree),
            vec![
                projects(),
                templates(),
                template("Band"),
                clips(),
                clip("a1_T1_B1")
            ]
        );

        tree.set_new_folder(true);
        assert_eq!(
            items(&tree)[..3],
            [projects(), BrowserItem::NewFolder, folder("a")]
        );
        assert_eq!(tree.rows()[1].depth, 1);
        assert_eq!(tree.selected(), Some(&projects()));
        assert_eq!(BrowserItem::NewFolder.parent(), Some(projects()));
        assert_eq!(BrowserItem::NewFolder.renamable_name(), None);

        tree.set_new_folder(false);
        assert!(!items(&tree).contains(&BrowserItem::NewFolder));
        assert_eq!(tree.selected(), Some(&projects()));
    }

    #[test]
    fn a_renamed_folder_stays_expanded() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        tree.carry_folder_expansion("a", "z");
        tree.reload(disk(&[("b", &[]), ("z", &["a1"])], &[], &[]));
        assert!(tree.is_expanded("z"));
        assert!(items(&tree).contains(&project(Some("z"), "a1")));
        // Collapsed, it stays collapsed.
        tree.carry_folder_expansion("b", "c");
        assert!(!tree.is_expanded("c"));
    }

    #[test]
    fn reload_keeps_expansion_and_hands_a_deleted_selection_to_the_next_row() {
        let mut tree = tree();
        tree.select(folder("a"));
        tree.expand();
        tree.select(project(Some("a"), "a2"));
        tree.reload(disk(&[("a", &["a1"]), ("b", &[])], &["loose"], &[]));
        assert!(tree.is_expanded("a"));
        assert_eq!(tree.selected(), Some(&project(Some("a"), "a1")));
    }

    #[test]
    fn reload_forgets_a_vanished_folders_expansion() {
        let mut tree = tree();
        tree.select(folder("b"));
        tree.expand();
        tree.reload(disk(&[("a", &[])], &[], &[]));
        assert!(!tree.is_expanded("b"));
        // The selection goes to the row now at `b`'s old index.
        assert_eq!(tree.selected(), Some(&templates()));
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
    fn only_projects_clips_and_folders_are_renamed() {
        assert_eq!(clip("riff").renamable_name(), Some("riff"));
        assert_eq!(clip("riff").renamed("lick".into()), Some(clip("lick")));
        assert_eq!(
            project(None, "loose").renamed("tight".into()),
            Some(project(None, "tight"))
        );
        assert_eq!(folder("a").renamable_name(), Some("a"));
        assert_eq!(folder("a").renamed("z".into()), Some(folder("z")));
        // The new folder's row has no name yet, and becomes the folder.
        assert_eq!(BrowserItem::NewFolder.renamable_name(), None);
        assert_eq!(
            BrowserItem::NewFolder.renamed("z".into()),
            Some(folder("z"))
        );
        for item in [
            projects(),
            clips(),
            BrowserItem::Plugin(plugin("Diva", "CLAP")),
            BrowserItem::Scanning,
        ] {
            assert_eq!(item.renamable_name(), None);
            assert_eq!(item.renamed("x".into()), None);
        }
    }

    #[test]
    fn browser_scroll_moves_only_as_far_as_needed() {
        let h = BROWSER_ROW_H;
        // Already wholly visible: stays put.
        assert_eq!(browser_scroll_to_show(0.0, 3, 10.0 * h), 0.0);
        // Below: its bottom lands on the list's bottom.
        assert_eq!(browser_scroll_to_show(0.0, 12, 10.0 * h), 3.0 * h);
        // Above: its top lands on the list's top.
        assert_eq!(browser_scroll_to_show(5.0 * h, 2, 10.0 * h), 2.0 * h);
        // Part-scrolled off the top: brought fully in.
        assert_eq!(browser_scroll_to_show(2.5 * h, 2, 10.0 * h), 2.0 * h);
        // Part-cut at the bottom (list not a whole number of rows tall).
        assert_eq!(browser_scroll_to_show(0.0, 9, 9.5 * h), 0.5 * h);
        // A list shorter than a row shows the row's top.
        assert_eq!(browser_scroll_to_show(0.0, 4, h * 0.5), 4.0 * h);
    }

    #[test]
    fn a_category_plus_sits_inside_the_right_end_of_its_row() {
        let row = browser_row_rect(pos2(0.0, 0.0), 240.0, 0, 0.0);
        let button = category_plus_rect(row);
        assert!(row.contains_rect(button));
        assert_eq!(button.max.x, row.max.x - CATEGORY_PLUS_RIGHT_X);
        assert_eq!(button.center().y, row.center().y);
        // Clear of the label, which starts at the row's left.
        assert!(button.min.x > row.min.x + row.width() * 0.5);
    }

    #[test]
    fn the_panel_width_is_whole_points_within_its_range() {
        assert_eq!(clamp_browser_width(300.4), 300.0);
        assert_eq!(clamp_browser_width(10.0), BROWSER_W_MIN);
        assert_eq!(clamp_browser_width(5000.0), BROWSER_W_MAX);
        assert_eq!(clamp_browser_width(f32::NAN), BROWSER_W_DEFAULT);
    }

    #[test]
    fn scroll_moves_only_as_far_as_needed() {
        assert_eq!(scroll_to_show(0, 3, 10), 0);
        assert_eq!(scroll_to_show(0, 12, 10), 3);
        assert_eq!(scroll_to_show(5, 2, 10), 2);
        assert_eq!(scroll_to_show(5, 14, 10), 5);
    }

    #[test]
    fn footer_and_its_toggle_stay_put_whether_the_panel_shows_or_not() {
        let window = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 800.0));
        for browser_w in [0.0, BROWSER_W_DEFAULT, BROWSER_W_MAX] {
            let mut canvas = window;
            canvas.max.x -= browser_w;
            let footer = footer_rect(canvas, 760.0, browser_w);
            // Drawn shifted right by the panel's width: the window's edges.
            let shift = vec2(browser_w, 0.0);
            let on_screen = footer.translate(shift);
            assert_eq!((on_screen.min.x, on_screen.max.x), (0.0, 1200.0));
            assert_eq!((footer.min.y, footer.max.y), (760.0, 800.0));
            // The toggle at its left edge, centred on the whole band.
            let toggle = browser_toggle_rect(footer).translate(shift);
            assert_eq!(toggle.min.x, BROWSER_TOGGLE_INSET_X);
            assert_eq!(toggle.center().y, 780.0);
            assert_eq!(toggle.size(), vec2(theme::STATUS_H, theme::STATUS_H));
        }
    }
}
