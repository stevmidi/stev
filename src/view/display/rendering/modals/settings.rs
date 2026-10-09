//! The settings modal (`settings_modal.rs`): one fixed-size panel with a tab
//! strip along its top and the showing tab below — MIDI (the input / output
//! port lists and the output-offset row) or Appearance (the theme list). The
//! layout is shared with the click hit-test
//! ([`settings_hit_at`](Display::settings_hit_at)), so the two can't drift.

use egui::Pos2;

use crate::view::display::midi_state::{MidiSettingsState, PORT_ROWS, PortList};
use crate::view::display::modal_focus::{PortSide, SettingsTab};
use crate::view::display::settings_modal::{SettingsHit, THEME_ROWS, window_start};

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
    /// Inset of a row's text from the content's left edge.
    const SETTINGS_TEXT_INSET: f32 = 8.0 * Self::M_SCALE;

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
    fn midi_tab_layout(content: Rect, midi: &MidiSettingsState) -> MidiTabLayout {
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
                let midi = Self::midi_tab_layout(layout.content, &self.midi);
                PortSide::ALL
                    .into_iter()
                    .zip(midi.ports)
                    .find_map(|(side, section)| {
                        let start = self.midi.list(side).window_start();
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
        };
        Some(hit.unwrap_or(SettingsHit::Panel))
    }

    /// Whether the pointer is on something in the settings panel a click
    /// acts on — for the pointing-hand cursor.
    pub(in crate::view::display) fn is_pointer_on_settings_item(&self) -> bool {
        self.canvas_pointer()
            .and_then(|p| self.settings_hit_at(p))
            .is_some_and(|hit| hit != SettingsHit::Panel)
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
        }
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
            let tint = if is_focused {
                theme::accent().gamma_multiply(0.15)
            } else {
                theme::fg_dim().gamma_multiply(0.08)
            };
            painter.rect_filled(row, CornerRadius::ZERO, tint);
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
        let focus = self.midi.focus;
        let layout = Self::midi_tab_layout(content, &self.midi);
        for (side, section) in PortSide::ALL.into_iter().zip(layout.ports) {
            let focused = focus == side.focus();
            Self::draw_settings_header(painter, section, side.label(), focused);
            Self::draw_port_list(painter, section, self.midi.list(side), focused);
        }

        // A number, not a list: ↑/↓ nudge it while focused, applied at once.
        // See `160-midi-out-offset.md` for why the delay exists.
        let offset_focused = focus == MidiSettingsFocus::OutOffset;
        let row = layout.offset.rows.row(0);
        Self::draw_settings_header(painter, layout.offset, "OUT OFFSET", offset_focused);
        Self::draw_settings_row(
            painter,
            row,
            &format!("{} ms", self.midi.out_offset_ms),
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
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;
    use egui::{Rect, pos2, vec2};

    use super::{Display, ListRows};
    use crate::view::display::midi_state::{MidiSettingsState, PORT_ROWS};
    use crate::view::display::settings_modal::THEME_ROWS;

    /// A canvas the size of a laptop window.
    fn canvas() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 900.0))
    }

    /// MIDI tab state with `inputs` / `outputs` ports, the input connected
    /// to `in_current` and the output to nothing.
    fn midi(inputs: usize, outputs: usize, in_current: Option<&str>) -> MidiSettingsState {
        let names = |n: usize| (0..n).map(|i| format!("port {i}")).collect();
        let mut midi = MidiSettingsState::new(
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
