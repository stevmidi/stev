//! The settings modal (`settings_modal.rs`): one fixed-size panel with a tab
//! strip along its top and the showing tab below — MIDI (the input / output
//! port lists and the output-offset row), Appearance (the theme list),
//! Keyboard (the search field over every command and fixed key,
//! `keyboard_tab.rs`) or About (the version, the links, the VST logo and,
//! on macOS, Check for updates, `about_tab.rs`). The layout is shared with the click hit-test
//! ([`settings_hit_at`](Display::settings_hit_at)), so the two can't drift.

use std::borrow::Cow;
use std::env::consts::ARCH;

use egui::emath::GuiRounding;
use egui::{Id, Pos2, Theme, Ui};

use crate::core::update_check::{CURRENT_VERSION, UpdateCheck};
use crate::view::display::about_tab::{AboutLink, UPDATE_CHECK, UpdateState, VST_LOGO_SIZE};

use crate::view::display::keyboard_tab::{
    KeyboardFocus, Recording, ShortcutLine, ShortcutRow, row_lines, shortcut_lines,
};
use crate::view::display::midi_tab::{MidiTab, PORT_ROWS, PortList};
use crate::view::display::modal_focus::{PortSide, SettingsTab};
use crate::view::display::settings_modal::{SettingsHit, THEME_ROWS, window_start};
use crate::view::display::text_field::{FieldStyle, MODAL_MARGIN};

use super::*;

/// Where the settings panel's frame parts sit on the canvas.
#[derive(Debug, Clone, PartialEq)]
struct SettingsLayout {
    /// The whole panel.
    panel: Rect,
    /// The tabs, in [`SettingsTab::ALL`] order.
    tabs: [Rect; SettingsTab::ALL.len()],
    /// The showing tab's area.
    content: Rect,
}

impl SettingsLayout {
    /// The y of the line under the tab strip — the tabs' bottom edge.
    fn strip_y(&self) -> f32 {
        self.tabs[0].max.y
    }
}

/// A column of equal rows: the first row's rect, stepped down by its height.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ListRows {
    /// The first row.
    first: Rect,
    /// How many rows.
    count: usize,
}

impl ListRows {
    /// Row `i`, counted from the top of the window.
    fn row(&self, i: usize) -> Rect {
        self.first
            .translate(vec2(0.0, i as f32 * self.first.height()))
    }

    /// The row under `p`, counted from the top of the window.
    fn hit(&self, p: Pos2) -> Option<usize> {
        (0..self.count).find(|&i| self.row(i).contains(p))
    }
}

/// One MIDI section: its header over its rows.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Section {
    /// The section header (`IN PORT`, `OUT PORT`, `OUT OFFSET`).
    header: Rect,
    /// The rows a click picks: the visible ports, or the offset value.
    rows: ListRows,
    /// The header and every slot under it — for a port list also the ghost
    /// row of a port that isn't plugged in, or the "No ports found" line.
    area: Rect,
}

/// Where the MIDI tab's parts sit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MidiTabLayout {
    /// The port sections, in [`PortSide::ALL`] order.
    ports: [Section; 2],
    /// The output-offset section: one value row.
    offset: Section,
}

/// Where the Keyboard tab's parts sit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct KeyboardTabLayout {
    /// The search field, across the top.
    field: Rect,
    /// The window the list shows through, a line per row.
    lines: ListRows,
    /// The foot row, under the list: Reset all at its right.
    foot: Rect,
}

/// Where the About tab's parts sit.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AboutTabLayout {
    /// The name line, in the title's larger size.
    name: Rect,
    /// The version and the licence, a line each.
    lines: ListRows,
    /// The links, a row each in [`AboutLink::LISTED`] order.
    links: ListRows,
    /// Check for updates.
    check: Rect,
    /// The check's answer, right of the button to the content's edge — a
    /// link when it found a newer release.
    status: Rect,
    /// The VST Compatible logo, at the foot.
    logo: Rect,
    /// The trademark sentence's two lines, right of the logo.
    trademark: ListRows,
}

impl Display {
    /// Panel width.
    const SETTINGS_W: f32 = 480.0 * Self::M_SCALE;
    /// Inset of the tab strip from the panel's top and left edges.
    const SETTINGS_TAB_INSET: f32 = 20.0 * Self::M_SCALE;
    /// Height of a tab, its label centred in it.
    const SETTINGS_TAB_H: f32 = 30.0 * Self::M_SCALE;
    /// Width of a tab.
    const SETTINGS_TAB_W: f32 = 110.0 * Self::M_SCALE;
    /// Gap between the tab strip's line and the tab's content.
    const SETTINGS_STRIP_GAP: f32 = 8.0 * Self::M_SCALE;
    /// Inset of the content from the panel's sides and bottom.
    const SETTINGS_PAD: f32 = 12.0 * Self::M_SCALE;
    /// Height of a MIDI section header.
    const SETTINGS_SECTION_H: f32 = 24.0 * Self::M_SCALE;
    /// Height of a list row — and the wheel travel that scrolls the theme
    /// list a row.
    pub(in crate::view::display) const SETTINGS_ROW_H: f32 = 22.0 * Self::M_SCALE;
    /// Gap between a Keyboard tab heading and the foot of its line.
    const SETTINGS_HEADING_PAD: f32 = 3.0 * Self::M_SCALE;
    /// Inset of a row's text from the content's left edge.
    const SETTINGS_TEXT_INSET: f32 = 8.0 * Self::M_SCALE;
    /// Width of the Keyboard tab's Reset all button: room for its confirm.
    const RESET_ALL_W: f32 = 240.0 * Self::M_SCALE;
    /// Radius of the dot by a rebound command's chords.
    const CHANGED_DOT_R: f32 = 2.5 * Self::M_SCALE;
    /// Width of the About tab's Check for updates.
    const CHECK_W: f32 = 130.0 * Self::M_SCALE;
    /// Line height of the trademark sentence: its two lines fit beside the
    /// logo.
    const ABOUT_NOTE_LINE_H: f32 = 1.3 * Self::FONT_HINT;

    /// The content height: the taller of the fullest MIDI tab (three
    /// headers; two port lists of `PORT_ROWS` plus a ghost row each; the
    /// offset row) and the theme window — so switching tabs never makes the
    /// panel jump.
    fn settings_content_h() -> f32 {
        let midi = 3.0 * Self::SETTINGS_SECTION_H
            + (2 * (PORT_ROWS + 1) + 1) as f32 * Self::SETTINGS_ROW_H;
        let themes = THEME_ROWS as f32 * Self::SETTINGS_ROW_H;
        midi.max(themes)
    }

    /// The panel's frame in the canvas `rect`, centred. Pure geometry.
    fn settings_layout(rect: Rect) -> SettingsLayout {
        let strip_h = Self::SETTINGS_TAB_INSET + Self::SETTINGS_TAB_H;
        let panel_h =
            strip_h + Self::SETTINGS_STRIP_GAP + Self::settings_content_h() + Self::SETTINGS_PAD;
        let panel = Rect::from_center_size(rect.center(), vec2(Self::SETTINGS_W, panel_h));
        let tabs = std::array::from_fn(|i| {
            Rect::from_min_size(
                pos2(
                    panel.min.x + Self::SETTINGS_TAB_INSET + i as f32 * Self::SETTINGS_TAB_W,
                    panel.min.y + Self::SETTINGS_TAB_INSET,
                ),
                vec2(Self::SETTINGS_TAB_W, Self::SETTINGS_TAB_H),
            )
        });
        let content = Rect::from_min_max(
            pos2(
                panel.min.x + Self::SETTINGS_PAD,
                panel.min.y + strip_h + Self::SETTINGS_STRIP_GAP,
            ),
            pos2(
                panel.max.x - Self::SETTINGS_PAD,
                panel.max.y - Self::SETTINGS_PAD,
            ),
        );
        SettingsLayout {
            panel,
            tabs,
            content,
        }
    }

    /// A section at `top` in `content`: its header, `rows` clickable rows,
    /// and `slots` rows of room in all (at least one). Pure geometry.
    fn settings_section(content: Rect, top: f32, rows: usize, slots: usize) -> Section {
        let header = Rect::from_min_size(
            pos2(content.min.x, top),
            vec2(content.width(), Self::SETTINGS_SECTION_H),
        );
        let first = Rect::from_min_size(
            pos2(content.min.x, header.max.y),
            vec2(content.width(), Self::SETTINGS_ROW_H),
        );
        let bottom = header.max.y + slots.max(1) as f32 * Self::SETTINGS_ROW_H;
        Section {
            header,
            rows: ListRows { first, count: rows },
            area: Rect::from_min_max(header.min, pos2(content.max.x, bottom)),
        }
    }

    /// The MIDI tab in `content` for `midi`'s port lists: the input section,
    /// the output section under it, then the offset. Pure geometry.
    fn midi_tab_layout(content: Rect, midi: &MidiTab) -> MidiTabLayout {
        let mut top = content.min.y;
        let ports = PortSide::ALL.map(|side| {
            let list = midi.list(side);
            let slots = list.visible() + list.missing().is_some() as usize;
            let section = Self::settings_section(content, top, list.visible(), slots);
            top = section.area.max.y;
            section
        });
        MidiTabLayout {
            ports,
            offset: Self::settings_section(content, top, 1, 1),
        }
    }

    /// The theme window's rows in `content`. Pure geometry.
    fn theme_rows(content: Rect) -> ListRows {
        ListRows {
            first: Rect::from_min_size(content.min, vec2(content.width(), Self::SETTINGS_ROW_H)),
            count: theme::theme_count().min(THEME_ROWS),
        }
    }

    /// The Keyboard tab in `content`: the search field, as many list lines
    /// as fit under it, and the foot row along the bottom. Pure geometry.
    fn keyboard_tab_layout(content: Rect) -> KeyboardTabLayout {
        let field = Rect::from_min_size(content.min, vec2(content.width(), Self::FIELD_H));
        let foot = Rect::from_min_max(
            pos2(content.min.x, content.max.y - Self::SETTINGS_ROW_H),
            content.max,
        );
        let top = field.max.y + Self::SETTINGS_STRIP_GAP;
        let bottom = foot.min.y - Self::SETTINGS_STRIP_GAP;
        let count = ((bottom - top) / Self::SETTINGS_ROW_H).floor() as usize;
        KeyboardTabLayout {
            field,
            lines: ListRows {
                first: Rect::from_min_size(
                    pos2(content.min.x, top),
                    vec2(content.width(), Self::SETTINGS_ROW_H),
                ),
                count,
            },
            foot,
        }
    }

    /// The Reset all button in the foot row `foot`, sized for its longest
    /// text and right-aligned. Pure geometry.
    fn reset_all_rect(foot: Rect) -> Rect {
        Rect::from_min_max(pos2(foot.max.x - Self::RESET_ALL_W, foot.min.y), foot.max)
    }

    /// The About tab in `content`: the name, version and licence from the
    /// top, a row's gap, the links, another gap and (macOS) Check for
    /// updates; the logo and the trademark sentence along the foot. Pure
    /// geometry.
    fn about_tab_layout(content: Rect) -> AboutTabLayout {
        let row_h = Self::SETTINGS_ROW_H;
        let row =
            |top: f32| Rect::from_min_size(pos2(content.min.x, top), vec2(content.width(), row_h));
        let name = Rect::from_min_size(content.min, vec2(content.width(), 2.0 * row_h));
        let lines = ListRows {
            first: row(name.max.y),
            count: 2,
        };
        let links = ListRows {
            first: row(lines.row(1).max.y + row_h),
            count: AboutLink::LISTED.len(),
        };
        let check_row = row(links.row(links.count - 1).max.y + row_h);
        let check = Rect::from_min_size(
            pos2(check_row.min.x + Self::SETTINGS_TEXT_INSET, check_row.min.y),
            vec2(Self::CHECK_W, row_h),
        );
        let logo = Rect::from_min_size(
            pos2(
                content.min.x + Self::SETTINGS_TEXT_INSET,
                content.max.y - VST_LOGO_SIZE.y,
            ),
            VST_LOGO_SIZE,
        );
        let trademark = ListRows {
            first: Rect::from_min_size(
                pos2(logo.max.x, logo.center().y - Self::ABOUT_NOTE_LINE_H),
                vec2(content.max.x - logo.max.x, Self::ABOUT_NOTE_LINE_H),
            ),
            count: 2,
        };
        AboutTabLayout {
            name,
            lines,
            links,
            check,
            status: Rect::from_min_max(pos2(check.max.x, check_row.min.y), check_row.max),
            logo,
            trademark,
        }
    }

    /// The Keyboard tab on the canvas, as drawn and hit-tested.
    fn keyboard_tab_on_canvas(&self) -> KeyboardTabLayout {
        Self::keyboard_tab_layout(Self::settings_layout(self.render.canvas_rect).content)
    }

    /// How many lines the Keyboard tab's list shows at once — the window its
    /// selection and wheel keep the scroll inside.
    pub(in crate::view::display) fn keyboard_visible_lines(&self) -> usize {
        self.keyboard_tab_on_canvas().lines.count
    }

    /// What canvas point `p` is on in the settings panel — `None` outside
    /// it.
    pub(in crate::view::display) fn settings_hit_at(&self, p: Pos2) -> Option<SettingsHit> {
        let layout = Self::settings_layout(self.render.canvas_rect);
        if !layout.panel.contains(p) {
            return None;
        }
        if let Some(tab) = SettingsTab::ALL
            .into_iter()
            .zip(layout.tabs)
            .find_map(|(tab, rect)| rect.contains(p).then_some(tab))
        {
            return Some(SettingsHit::Tab(tab));
        }
        let hit = match self.settings.tab {
            SettingsTab::Midi => {
                let midi = Self::midi_tab_layout(layout.content, &self.settings.midi);
                PortSide::ALL
                    .into_iter()
                    .zip(midi.ports)
                    .find_map(|(side, section)| {
                        let start = self.settings.midi.list(side).window_start();
                        section
                            .rows
                            .hit(p)
                            .map(|i| SettingsHit::Port(side, start + i))
                            .or_else(|| {
                                section
                                    .area
                                    .contains(p)
                                    .then_some(SettingsHit::Section(side.focus()))
                            })
                    })
                    .or_else(|| {
                        midi.offset
                            .area
                            .contains(p)
                            .then_some(SettingsHit::Section(MidiSettingsFocus::OutOffset))
                    })
            }
            SettingsTab::Appearance => Self::theme_rows(layout.content).hit(p).map(|i| {
                SettingsHit::Theme(
                    window_start(self.settings.theme_scroll, theme::theme_count(), THEME_ROWS) + i,
                )
            }),
            SettingsTab::Keyboard => {
                let tab = Self::keyboard_tab_layout(layout.content);
                if tab.field.contains(p) {
                    return Some(SettingsHit::Search);
                }
                if Self::reset_all_rect(tab.foot).contains(p) {
                    return Some(SettingsHit::ResetAll);
                }
                let window = tab.lines;
                let lines = shortcut_lines(&self.settings.keys.search.text, &self.keymap);
                let start = window_start(self.settings.keys.scroll, lines.len(), window.count);
                let line = window.hit(p)? + start;
                row_lines(&lines)
                    .binary_search(&line)
                    .ok()
                    .map(|row| SettingsHit::Shortcut {
                        row,
                        keys: p.x >= window.first.center().x,
                    })
            }
            SettingsTab::About => self.about_hit_at(Self::about_tab_layout(layout.content), p),
        };
        Some(hit.unwrap_or(SettingsHit::Panel))
    }

    /// What `p` is on in the About tab laid out as `layout`: a link's row,
    /// Check for updates (not while a check runs), or the check's answer
    /// when it links a newer release.
    fn about_hit_at(&self, layout: AboutTabLayout, p: Pos2) -> Option<SettingsHit> {
        if let Some(i) = layout.links.hit(p) {
            return Some(SettingsHit::Link(AboutLink::LISTED[i]));
        }
        let about = &self.settings.about;
        if UPDATE_CHECK && layout.check.contains(p) && !about.is_checking() {
            return Some(SettingsHit::CheckForUpdates);
        }
        (layout.status.contains(p) && about.release_url().is_some())
            .then_some(SettingsHit::Link(AboutLink::Release))
    }

    /// Whether the pointer is on something in the settings panel a click
    /// acts on — for the pointing-hand cursor. Not the search field, which
    /// keeps egui's text cursor.
    pub(in crate::view::display) fn is_pointer_on_settings_item(&self) -> bool {
        self.canvas_pointer()
            .and_then(|p| self.settings_hit_at(p))
            .is_some_and(|hit| !matches!(hit, SettingsHit::Panel | SettingsHit::Search))
    }

    /// Paints the settings modal into the canvas `rect`.
    pub(in crate::view::display::rendering) fn draw_settings_view(
        &self,
        painter: &Painter,
        rect: Rect,
    ) {
        let layout = Self::settings_layout(rect);
        Self::draw_modal_panel(painter, layout.panel);

        for (tab, tab_rect) in SettingsTab::ALL.into_iter().zip(layout.tabs) {
            let active = tab == self.settings.tab;
            let label = painter.text(
                pos2(tab_rect.min.x, tab_rect.center().y),
                Align2::LEFT_CENTER,
                tab.label(),
                FontId::proportional(Self::FONT_TITLE),
                accent_if(active, theme::fg_dim()),
            );
            if active {
                painter.hline(
                    label.x_range(),
                    layout.strip_y() - 1.0,
                    Stroke::new(2.0_f32, theme::accent()),
                );
            }
        }
        painter.hline(
            (layout.panel.min.x + Self::SETTINGS_TAB_INSET)
                ..=(layout.panel.max.x - Self::SETTINGS_TAB_INSET),
            layout.strip_y(),
            Self::modal_outline(),
        );

        match self.settings.tab {
            SettingsTab::Midi => self.draw_midi_tab(painter, layout.content),
            SettingsTab::Appearance => self.draw_appearance_tab(painter, layout.content),
            SettingsTab::Keyboard => self.draw_keyboard_tab(painter, layout.content),
            SettingsTab::About => self.draw_about_tab(painter, layout.content),
        }
    }

    /// The About tab: the name, the version with the architecture the
    /// binary runs as (which tells an Apple Silicon report from an Intel
    /// one), the licence, the links, (macOS) Check for updates with its
    /// answer, and the VST Compatible logo with Steinberg's trademark
    /// sentence — the standard logo on a dark theme, the negative on a
    /// light one, at the screen's scale.
    fn draw_about_tab(&self, painter: &Painter, content: Rect) {
        let layout = Self::about_tab_layout(content);
        Self::draw_settings_text(
            painter,
            layout.name,
            "Stev",
            2.0 * Self::FONT_TITLE,
            theme::fg(),
        );
        let version = format!("Version {CURRENT_VERSION} · {ARCH}");
        let lines = [version.as_str(), "Licence: MIT OR Apache-2.0"];
        for (i, text) in lines.into_iter().enumerate() {
            let row = layout.lines.row(i);
            Self::draw_settings_text(painter, row, text, Self::FONT_LIST, theme::fg_dim());
        }
        for (i, link) in AboutLink::LISTED.into_iter().enumerate() {
            let row = layout.links.row(i);
            Self::draw_settings_text(painter, row, link.label(), Self::FONT_LIST, theme::accent());
        }
        if UPDATE_CHECK {
            self.draw_update_check(painter, layout);
        }

        let ctx = painter.ctx();
        let ppp = painter.pixels_per_point();
        let texture = self
            .settings
            .about
            .logo
            .texture(ctx, ctx.theme() == Theme::Dark, ppp > 1.5);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(
            texture.id(),
            layout.logo.round_to_pixels(ppp),
            uv,
            Color32::WHITE,
        );
        let trademark = [
            "VST is a registered trademark of",
            "Steinberg Media Technologies GmbH.",
        ];
        for (i, text) in trademark.into_iter().enumerate() {
            let row = layout.trademark.row(i);
            Self::draw_settings_text(painter, row, text, Self::FONT_HINT, theme::fg_dim());
        }
    }

    /// Check for updates, framed like the project dialogs' buttons ("Checking…"
    /// and dim while a check runs), and its answer beside it: up to date, a
    /// newer version as a link to its release, or GitHub out of reach.
    fn draw_update_check(&self, painter: &Painter, layout: AboutTabLayout) {
        let about = &self.settings.about;
        let checking = about.is_checking();
        let (label, color) = if checking {
            ("Checking…", theme::fg_dim())
        } else {
            ("Check for updates", theme::fg())
        };
        Self::draw_modal_button(painter, layout.check, label, false, color);
        let (text, color) = match &about.update {
            UpdateState::Done(UpdateCheck::UpToDate) => (
                format!("You're up to date ({CURRENT_VERSION})"),
                theme::fg_dim(),
            ),
            UpdateState::Done(UpdateCheck::Available { version, .. }) => (
                format!("Version {version} is available — view the release"),
                theme::accent(),
            ),
            UpdateState::Done(UpdateCheck::Failed) => {
                ("Couldn't reach GitHub".to_owned(), theme::track_mute())
            }
            UpdateState::Idle | UpdateState::Requested | UpdateState::Checking(_) => return,
        };
        Self::draw_settings_text(painter, layout.status, &text, Self::FONT_LIST, color);
    }

    /// Paints `text` on one line at the left of `rect`, cut with an ellipsis
    /// to fit it — every line of the modal's content.
    fn draw_settings_text(painter: &Painter, rect: Rect, text: &str, size: f32, color: Color32) {
        Self::draw_modal_text(
            painter,
            pos2(rect.min.x + Self::SETTINGS_TEXT_INSET, rect.center().y),
            text,
            FontId::proportional(size),
            color,
            rect.width() - 2.0 * Self::SETTINGS_TEXT_INSET,
        );
    }

    /// Paints the cursor row's tint over `row`: accent while its list has
    /// the focus, a faint `fg_dim` otherwise.
    fn draw_settings_cursor(painter: &Painter, row: Rect, is_focused: bool) {
        let tint = if is_focused {
            theme::accent().gamma_multiply(0.15)
        } else {
            theme::fg_dim().gamma_multiply(0.08)
        };
        painter.rect_filled(row, CornerRadius::ZERO, tint);
    }

    /// Paints one list row: the cursor's tint (accent when its list has the
    /// focus), the connected / active bar along its left edge, and `text`.
    fn draw_settings_row(
        painter: &Painter,
        row: Rect,
        text: &str,
        is_cursor: bool,
        is_focused: bool,
        is_active: bool,
    ) {
        if is_cursor {
            Self::draw_settings_cursor(painter, row, is_focused);
        }
        if is_active {
            painter.vline(
                row.min.x,
                (row.min.y + 2.0)..=(row.max.y - 2.0),
                Stroke::new(3.0_f32, theme::region()),
            );
        }
        let color = if is_active {
            theme::region()
        } else {
            accent_if(is_cursor && is_focused, theme::fg())
        };
        Self::draw_settings_text(painter, row, text, Self::FONT_LIST, color);
    }

    /// Paints a MIDI section header, accented while its row has the focus.
    fn draw_settings_header(painter: &Painter, section: Section, label: &str, focused: bool) {
        let color = accent_if(focused, theme::fg_dim());
        Self::draw_settings_text(painter, section.header, label, Self::FONT_HINT, color);
    }

    /// Paints one port list: a window scrolled to keep its cursor in view,
    /// and a ghost row for a connected port that isn't plugged in.
    fn draw_port_list(painter: &Painter, section: Section, list: &PortList, focused: bool) {
        let dim_line = |i: usize, text: &str| {
            let row = section.rows.row(i);
            Self::draw_settings_text(painter, row, text, Self::FONT_LIST, theme::fg_dim());
        };
        let missing = list.missing();
        if list.ports.is_empty() && missing.is_none() {
            dim_line(0, "No ports found");
            return;
        }
        let current = list.current_index();
        let start = list.window_start();
        for i in 0..section.rows.count {
            let idx = start + i;
            Self::draw_settings_row(
                painter,
                section.rows.row(i),
                &list.ports[idx],
                idx == list.selection,
                focused,
                current == Some(idx),
            );
        }
        if let Some(missing) = missing {
            dim_line(section.rows.count, &format!("○ {missing} (not connected)"));
        }
    }

    /// The MIDI tab: the input and output port lists and the output-offset
    /// row.
    fn draw_midi_tab(&self, painter: &Painter, content: Rect) {
        let focus = self.settings.midi.focus;
        let layout = Self::midi_tab_layout(content, &self.settings.midi);
        for (side, section) in PortSide::ALL.into_iter().zip(layout.ports) {
            let focused = focus == side.focus();
            Self::draw_settings_header(painter, section, side.label(), focused);
            Self::draw_port_list(painter, section, self.settings.midi.list(side), focused);
        }

        // A number, not a list: ↑/↓ nudge it while focused, applied at once.
        // See `160-midi-out-offset.md` for why the delay exists.
        let offset_focused = focus == MidiSettingsFocus::OutOffset;
        let row = layout.offset.rows.row(0);
        Self::draw_settings_header(painter, layout.offset, "OUT OFFSET", offset_focused);
        Self::draw_settings_row(
            painter,
            row,
            &format!("{} ms", self.settings.midi.out_offset_ms),
            offset_focused,
            true,
            false,
        );
        painter.text(
            pos2(row.max.x - Self::SETTINGS_TEXT_INSET, row.center().y),
            Align2::RIGHT_CENTER,
            "delays MIDI OUT to match instrument tracks",
            FontId::proportional(Self::FONT_HINT),
            theme::fg_dim(),
        );
    }

    /// The Appearance tab: a window over the theme list, the active theme
    /// highlighted — always the saved one, since a pick saves at once.
    fn draw_appearance_tab(&self, painter: &Painter, content: Rect) {
        let rows = Self::theme_rows(content);
        let count = theme::theme_count();
        let active = theme::active_theme_index();
        let start = window_start(self.settings.theme_scroll, count, THEME_ROWS);
        for (i, idx) in (start..count).take(rows.count).enumerate() {
            let is_active = idx == active;
            Self::draw_settings_row(
                painter,
                rows.row(i),
                theme::theme_name(idx),
                is_active,
                true,
                is_active,
            );
        }
    }

    /// The Keyboard tab's list under its search field (the field itself is
    /// [`show_keyboard_search`](Self::show_keyboard_search)): the rows the
    /// search matches under their groups' headings, the selected row
    /// tinted (accent while the list has the keyboard, faint while the
    /// field has it), and Reset all at the foot. A heading sits at the foot of its
    /// line, so the room above it starts the group.
    fn draw_keyboard_tab(&self, painter: &Painter, content: Rect) {
        let layout = Self::keyboard_tab_layout(content);
        self.draw_reset_all(painter, layout.foot);
        let window = layout.lines;
        let keys = &self.settings.keys;
        let lines = shortcut_lines(&keys.search.text, &self.keymap);
        if lines.is_empty() {
            let row = window.row(0);
            Self::draw_settings_text(
                painter,
                row,
                "No shortcut matches",
                Self::FONT_LIST,
                theme::fg_dim(),
            );
            return;
        }
        let selected = row_lines(&lines).get(keys.selection).copied();
        let list_focused = keys.focus == KeyboardFocus::List;
        let start = window_start(keys.scroll, lines.len(), window.count);
        for (i, line) in lines.iter().enumerate().skip(start).take(window.count) {
            let rect = window.row(i - start);
            match line {
                ShortcutLine::Header(category) => {
                    painter.text(
                        pos2(
                            rect.min.x + Self::SETTINGS_TEXT_INSET,
                            rect.max.y - Self::SETTINGS_HEADING_PAD,
                        ),
                        Align2::LEFT_BOTTOM,
                        category.to_uppercase(),
                        FontId::proportional(Self::FONT_HINT),
                        theme::fg_dim(),
                    );
                }
                ShortcutLine::Row(row) => {
                    let recording = keys
                        .recording
                        .filter(|recording| row.action == Some(recording.action));
                    let is_selected = selected == Some(i);
                    Self::draw_shortcut_row(
                        painter,
                        rect,
                        row,
                        is_selected,
                        list_focused,
                        recording,
                    );
                }
            }
        }
    }

    /// Paints Reset all at the right of the foot row: dim while every
    /// command is on its defaults, accented while it waits for the
    /// confirming second click.
    fn draw_reset_all(&self, painter: &Painter, foot: Rect) {
        let armed = self.settings.keys.reset_all_armed;
        let (text, color) = if armed {
            ("Reset every shortcut? Click again", theme::accent())
        } else if self.keymap.is_all_default() {
            ("Reset all", theme::fg_dim())
        } else {
            ("Reset all", theme::fg())
        };
        Self::draw_settings_text_right(painter, foot, foot.max.x, text, Self::FONT_LIST, color);
    }

    /// Paints `text` on one line in `rect`, its right edge an inset short of
    /// `right`; returns where it went, so the next text can sit left of it.
    /// The right-aligned sibling of
    /// [`draw_settings_text`](Self::draw_settings_text).
    fn draw_settings_text_right(
        painter: &Painter,
        rect: Rect,
        right: f32,
        text: impl ToString,
        size: f32,
        color: Color32,
    ) -> Rect {
        painter.text(
            pos2(right - Self::SETTINGS_TEXT_INSET, rect.center().y),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(size),
            color,
        )
    }

    /// The text in place of a row's keys: what the recorder waits for while
    /// the row records, else the keys (`unbound` for a command with none).
    fn shortcut_keys_text(row: &ShortcutRow, recording: Option<Recording>) -> Cow<'_, str> {
        match recording {
            Some(Recording {
                replacing: Some((_, other)),
                ..
            }) => Cow::Owned(format!("Replaces {} — press again", other.info().label)),
            Some(_) => Cow::Borrowed("Press a key…"),
            None => Cow::Borrowed(&row.keys),
        }
    }

    /// Paints one row of the Keyboard tab: the selection's tint (accent
    /// only while the list `is_focused`), the label
    /// on the left, cut short of the keys, and the keys on the right. A
    /// fixed key's keys read `fg_dim` — listed, not rebindable — and its
    /// label stays as legible as any. An unbound command reads `unbound`
    /// in `track_mute`; a recording row reads what it waits
    /// for, accented. A dot left of the keys marks a command off its
    /// defaults, and a dim note one the computer keyboard shadows while it
    /// plays.
    fn draw_shortcut_row(
        painter: &Painter,
        rect: Rect,
        row: &ShortcutRow,
        is_selected: bool,
        is_focused: bool,
        recording: Option<Recording>,
    ) {
        if is_selected || recording.is_some() {
            Self::draw_settings_cursor(painter, rect, is_focused);
        }
        let color = accent_if(is_selected && is_focused, theme::fg());
        let keys_color = if recording.is_some() {
            theme::accent()
        } else if row.action.is_none() {
            theme::fg_dim()
        } else if row.unbound {
            theme::track_mute()
        } else {
            color
        };
        let keys = Self::draw_settings_text_right(
            painter,
            rect,
            rect.max.x,
            Self::shortcut_keys_text(row, recording),
            Self::FONT_LIST,
            keys_color,
        );
        let mut keys_left = keys.min.x;
        if recording.is_none() && row.changed {
            let r = Self::CHANGED_DOT_R;
            let centre = pos2(keys_left - Self::SETTINGS_TEXT_INSET, rect.center().y);
            painter.circle_filled(centre, r, theme::accent());
            keys_left = centre.x - r;
        }
        if recording.is_none() && row.piano {
            let note = Self::draw_settings_text_right(
                painter,
                rect,
                keys_left,
                "not while playing notes",
                Self::FONT_HINT,
                theme::fg_dim(),
            );
            keys_left = note.min.x;
        }
        let label_x = rect.min.x + Self::SETTINGS_TEXT_INSET;
        Self::draw_modal_text(
            painter,
            pos2(label_x, rect.center().y),
            row.label,
            FontId::proportional(Self::FONT_LIST),
            color,
            keys_left - Self::SETTINGS_TEXT_INSET - label_x,
        );
    }

    /// Shows the Keyboard tab's search field while that tab is up, in the
    /// canvas `ui`. It holds the egui focus while it has the tab's keyboard
    /// (`KeyboardFocus::Search`), so typing lands in it; while the list has
    /// it, or a row records a shortcut, the focus is given up before the
    /// field is shown, so this frame's keys never reach it and it shows no
    /// caret. A change to its text puts the selection and the list back at
    /// the top.
    pub(in crate::view::display) fn show_keyboard_search(&mut self, ui: &mut Ui) {
        if !self.keyboard_tab_showing() {
            return;
        }
        let rect = self.keyboard_tab_on_canvas().field;
        let keys = &mut self.settings.keys;
        let id = Id::new("keyboard-search");
        let focused = keys.focus == KeyboardFocus::Search && keys.recording.is_none();
        if !focused {
            ui.memory_mut(|memory| memory.surrender_focus(id));
        }
        let style =
            FieldStyle::new(FontId::proportional(Self::FONT_LIST), MODAL_MARGIN).hint("Search");
        let response = keys.search.show(ui, id, rect, style);
        if focused && !response.has_focus() {
            response.request_focus();
        }
        if response.changed() {
            keys.reset_list();
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;
    use egui::{Rect, pos2, vec2};

    use super::{Display, ListRows};
    use crate::view::display::midi_tab::{MidiTab, PORT_ROWS};
    use crate::view::display::settings_modal::THEME_ROWS;

    /// A canvas the size of a laptop window.
    fn canvas() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 900.0))
    }

    /// MIDI tab state with `inputs` / `outputs` ports, the input connected
    /// to `in_current` and the output to nothing.
    fn midi(inputs: usize, outputs: usize, in_current: Option<&str>) -> MidiTab {
        let names = |n: usize| (0..n).map(|i| format!("port {i}")).collect();
        let mut midi = MidiTab::new(
            unbounded().0,
            unbounded().0,
            in_current.map(str::to_owned),
            None,
            0,
        );
        midi.input.set_ports(names(inputs));
        midi.output.set_ports(names(outputs));
        midi
    }

    #[test]
    fn the_panel_is_centred_and_its_content_sits_under_the_tabs() {
        let layout = Display::settings_layout(canvas());
        assert_eq!(layout.panel.center(), canvas().center());
        assert!(layout.panel.contains_rect(layout.content));
        for tab in layout.tabs {
            assert!(layout.panel.contains_rect(tab));
            assert!(tab.max.y <= layout.strip_y());
        }
        assert!(layout.content.min.y > layout.strip_y());
        assert!(layout.tabs[0].max.x <= layout.tabs[1].min.x);
    }

    #[test]
    fn the_fullest_midi_tab_and_the_theme_window_both_fit_the_content() {
        let content = Display::settings_layout(canvas()).content;
        let mut full = midi(PORT_ROWS + 3, PORT_ROWS + 3, Some("unplugged"));
        full.output.current = Some("unplugged".into());
        let layout = Display::midi_tab_layout(content, &full);
        assert!(layout.offset.area.max.y <= content.max.y + 0.01);
        let themes = Display::theme_rows(content);
        assert!(themes.row(THEME_ROWS - 1).max.y <= content.max.y + 0.01);
    }

    #[test]
    fn the_midi_sections_stack_without_overlapping() {
        let content = Display::settings_layout(canvas()).content;
        let layout = Display::midi_tab_layout(content, &midi(2, 0, None));
        let [input, output] = layout.ports;
        assert_eq!(input.area.min.y, content.min.y);
        assert_eq!(output.area.min.y, input.area.max.y);
        assert_eq!(layout.offset.area.min.y, output.area.max.y);
        // An empty list still keeps one slot, for "No ports found".
        assert_eq!(
            output.area.height(),
            Display::SETTINGS_SECTION_H + Display::SETTINGS_ROW_H
        );
    }

    #[test]
    fn a_ghost_row_takes_a_slot_but_is_not_a_port_row() {
        let content = Display::settings_layout(canvas()).content;
        let [input, _] = Display::midi_tab_layout(content, &midi(1, 1, Some("unplugged"))).ports;
        let ghost = input.rows.row(1);
        assert!(input.area.contains(ghost.center()));
        assert_eq!(input.rows.hit(ghost.center()), None);
        assert_eq!(input.rows.hit(input.rows.row(0).center()), Some(0));
    }

    #[test]
    fn the_keyboard_tab_lists_under_its_field_inside_the_content() {
        let content = Display::settings_layout(canvas()).content;
        let layout = Display::keyboard_tab_layout(content);
        assert_eq!(layout.field.min, content.min);
        assert!(layout.lines.first.min.y > layout.field.max.y);
        assert!(layout.lines.count >= 10);
        assert!(layout.lines.row(layout.lines.count - 1).max.y <= layout.foot.min.y + 0.01);
        assert_eq!(layout.foot.max, content.max);
        let reset_all = Display::reset_all_rect(layout.foot);
        assert!(layout.foot.contains_rect(reset_all));
    }

    #[test]
    fn the_about_tab_fits_the_content_without_overlapping() {
        let content = Display::settings_layout(canvas()).content;
        let layout = Display::about_tab_layout(content);
        assert!(content.contains_rect(layout.logo));
        assert_eq!(layout.logo.max.y, content.max.y);
        let last_link = layout.links.row(layout.links.count - 1);
        assert!(layout.lines.row(1).max.y < layout.links.first.min.y);
        assert!(last_link.max.y < layout.trademark.first.min.y);
        assert!(layout.trademark.first.min.y >= layout.logo.min.y);
        assert!(layout.trademark.row(1).max.y <= layout.logo.max.y);
        assert!(last_link.max.y < layout.check.min.y);
        assert!(layout.check.max.y < layout.logo.min.y);
        assert_eq!(layout.status.min.x, layout.check.max.x);
        assert_eq!(layout.status.max.x, content.max.x);
    }

    #[test]
    fn a_list_row_hit_counts_from_the_top_of_the_window() {
        let rows = ListRows {
            first: Rect::from_min_size(pos2(10.0, 100.0), vec2(200.0, 20.0)),
            count: 3,
        };
        assert_eq!(rows.hit(pos2(50.0, 105.0)), Some(0));
        assert_eq!(rows.hit(pos2(50.0, 145.0)), Some(2));
        assert_eq!(rows.hit(pos2(50.0, 165.0)), None);
        assert_eq!(rows.hit(pos2(5.0, 105.0)), None);
    }
}
