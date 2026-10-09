//! The help overlay (`help_overlay.rs`): one panel titled like the project
//! dialogs, the [`HELP`] table below in three columns — each a stack of
//! titled sections of key / action rows. The layout is shared with the
//! scroll range ([`help_overflow`](Display::help_overflow)), so a page
//! taller than the window scrolls exactly as far as it overflows.

use crate::view::display::help_overlay::{HELP, HelpSection, key_label};

use super::*;

/// Where the help panel's parts sit on the canvas.
#[derive(Debug, Clone, Copy)]
struct HelpLayout {
    /// The whole panel.
    panel: Rect,
    /// The window the page shows through, under the title.
    content: Rect,
    /// The page's full height — taller than `content` only in a window too
    /// short to show it whole.
    page_h: f32,
}

impl HelpLayout {
    /// How far the page overflows the window — the scroll range.
    fn overflow(&self) -> f32 {
        (self.page_h - self.content.height()).max(0.0)
    }
}

impl Display {
    /// Width of a column.
    const HELP_COL_W: f32 = 380.0;
    /// Width of a column's keys, left of the actions.
    const HELP_KEYS_W: f32 = 130.0;
    /// Gap between columns.
    const HELP_COL_GAP: f32 = 24.0;
    /// Inset of the page from the panel's sides and bottom.
    const HELP_PAD: f32 = 20.0;
    /// Height of the title band, from the panel's top to the page.
    const HELP_TITLE_H: f32 = 70.0;
    /// Height of a section's title row.
    const HELP_SECTION_H: f32 = 30.0;
    /// Gap between two sections in a column.
    const HELP_SECTION_GAP: f32 = 12.0;
    /// Height of a key / action row.
    const HELP_ROW_H: f32 = 22.0;
    /// Font size of the keys (monospace) and the actions.
    const HELP_FONT: f32 = 14.0;
    /// Margin kept between the panel and the canvas edges.
    const HELP_MARGIN: f32 = 16.0;

    /// The height of a column of `sections`. Pure geometry.
    fn help_column_h(sections: &[HelpSection]) -> f32 {
        let rows: usize = sections.iter().map(|section| section.rows.len()).sum();
        sections.len() as f32 * Self::HELP_SECTION_H
            + rows as f32 * Self::HELP_ROW_H
            + sections.len().saturating_sub(1) as f32 * Self::HELP_SECTION_GAP
    }

    /// The panel in the canvas `rect`, centred, as tall as the page or as
    /// the canvas allows. Pure geometry.
    fn help_layout(rect: Rect) -> HelpLayout {
        let page_h = HELP
            .iter()
            .map(|column| Self::help_column_h(column))
            .fold(0.0, f32::max);
        let panel_w = 2.0 * Self::HELP_PAD
            + HELP.len() as f32 * Self::HELP_COL_W
            + (HELP.len() - 1) as f32 * Self::HELP_COL_GAP;
        let panel_h = (Self::HELP_TITLE_H + page_h + Self::HELP_PAD)
            .min(rect.height() - 2.0 * Self::HELP_MARGIN);
        let panel = Rect::from_center_size(rect.center(), vec2(panel_w, panel_h));
        let content = Rect::from_min_max(
            pos2(
                panel.min.x + Self::HELP_PAD,
                panel.min.y + Self::HELP_TITLE_H,
            ),
            pos2(panel.max.x - Self::HELP_PAD, panel.max.y - Self::HELP_PAD),
        );
        HelpLayout {
            panel,
            content,
            page_h,
        }
    }

    /// How far the page overflows the panel's window — the scroll range.
    pub(in crate::view::display) fn help_overflow(&self) -> f32 {
        Self::help_layout(self.render.canvas_rect).overflow()
    }

    /// Paints the help overlay into the canvas `rect`: the column of the
    /// pane with the keyboard has its section titles accented.
    pub(in crate::view::display::rendering) fn draw_help_view(
        &self,
        painter: &Painter,
        rect: Rect,
    ) {
        let layout = Self::help_layout(rect);
        Self::draw_modal_frame(
            painter,
            rect,
            layout.panel.width(),
            layout.panel.height(),
            "Keyboard shortcuts",
        );
        let page = painter.with_clip_rect(layout.content);
        let focused = self.focused_pane();
        let scroll = self.help.scroll.min(layout.overflow());
        for (i, column) in HELP.iter().enumerate() {
            let x = layout.content.min.x + i as f32 * (Self::HELP_COL_W + Self::HELP_COL_GAP);
            let mut y = layout.content.min.y - scroll;
            for section in column.iter() {
                let title = accent_if(section.pane == Some(focused), theme::fg_dim());
                page.text(
                    pos2(x, y + Self::HELP_SECTION_H * 0.5),
                    Align2::LEFT_CENTER,
                    section.title.to_uppercase(),
                    FontId::proportional(Self::FONT_HINT),
                    title,
                );
                y += Self::HELP_SECTION_H;
                for &(keys, action) in section.rows {
                    let mid_y = y + Self::HELP_ROW_H * 0.5;
                    page.text(
                        pos2(x, mid_y),
                        Align2::LEFT_CENTER,
                        key_label(keys),
                        FontId::monospace(Self::HELP_FONT),
                        theme::fg(),
                    );
                    Self::draw_modal_text(
                        &page,
                        pos2(x + Self::HELP_KEYS_W, mid_y),
                        action,
                        FontId::proportional(Self::HELP_FONT),
                        theme::fg_dim(),
                        Self::HELP_COL_W - Self::HELP_KEYS_W,
                    );
                    y += Self::HELP_ROW_H;
                }
                y += Self::HELP_SECTION_GAP;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};

    use super::Display;
    use crate::view::display::help_overlay::{HELP, key_label_for};

    #[test]
    fn the_page_fits_a_1280_by_800_window_unscrolled() {
        // The canvas is the whole window: the modal covers the header too.
        let canvas = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0));
        let layout = Display::help_layout(canvas);
        assert!(canvas.contains_rect(layout.panel));
        assert_eq!(layout.panel.center(), canvas.center());
        assert!(layout.page_h <= layout.content.height() + 0.01);
    }

    #[test]
    fn a_short_window_keeps_the_panel_inside_and_scrolls_the_rest() {
        let canvas = Rect::from_min_size(pos2(0.0, 0.0), vec2(1400.0, 400.0));
        let layout = Display::help_layout(canvas);
        assert!(layout.panel.height() <= 400.0 - 2.0 * Display::HELP_MARGIN + 0.01);
        assert!(layout.page_h > layout.content.height());
    }

    #[test]
    fn every_key_label_fits_its_column() {
        // The bundled Hack advances 0.602 em a glyph; keep a gap of 8 points
        // before the action.
        let max_chars = ((Display::HELP_KEYS_W - 8.0) / (Display::HELP_FONT * 0.61)) as usize;
        for section in HELP.iter().flat_map(|column| column.iter()) {
            for &(keys, _) in section.rows {
                for mac in [true, false] {
                    let label = key_label_for(keys, mac);
                    assert!(label.chars().count() <= max_chars, "{label} is too long");
                }
            }
        }
    }
}
