//! The plugin-restore overlay (`instrument_restore.rs`): while a project
//! being opened loads its plugins, one per frame, a small panel over the
//! still-open project, titled with the new one's name, names the plugin
//! loading next, counts the step ("3 of 8") and fills a bar. Each load still
//! blocks the main thread, so nothing here animates; the panel moves on
//! between loads.

use crate::view::display::instrument_restore::InstrumentRestore;

use super::*;

impl Display {
    /// Panel width.
    const RESTORE_W: f32 = 420.0 * Self::M_SCALE;
    /// Panel height.
    const RESTORE_H: f32 = 130.0 * Self::M_SCALE;
    /// Inset of the contents from the panel's sides.
    const RESTORE_PAD: f32 = 20.0 * Self::M_SCALE;
    /// The line naming the plugin, from the panel's top.
    const RESTORE_LINE_Y: f32 = 70.0 * Self::M_SCALE;
    /// The progress bar's top, from the panel's top.
    const RESTORE_BAR_Y: f32 = 94.0 * Self::M_SCALE;
    /// The progress bar's height.
    const RESTORE_BAR_H: f32 = 4.0 * Self::M_SCALE;

    /// Paints the plugin-restore overlay into the canvas `rect`.
    pub(in crate::view::display::rendering) fn draw_instrument_restore_view(
        &self,
        painter: &Painter,
        rect: Rect,
    ) {
        let Some((restore, name)) = self.instrument_restore() else {
            return;
        };
        let title = format!("Opening {name}");
        let (px, py) =
            Self::draw_modal_frame(painter, rect, Self::RESTORE_W, Self::RESTORE_H, &title);

        let (step, total) = restore.step();
        let count = format!("{step} of {total}");
        let count_font = FontId::proportional(Self::FONT_HINT);
        let count_w = painter
            .layout_no_wrap(count.clone(), count_font.clone(), theme::fg_dim())
            .size()
            .x;
        let right = px + Self::RESTORE_W - Self::RESTORE_PAD;
        let line_y = py + Self::RESTORE_LINE_Y;
        painter.text(
            pos2(right, line_y),
            Align2::RIGHT_CENTER,
            count,
            count_font,
            theme::fg_dim(),
        );
        let left = px + Self::RESTORE_PAD;
        Self::draw_modal_text(
            painter,
            pos2(left, line_y),
            &self.restore_line(restore),
            FontId::proportional(Self::FONT_LIST),
            theme::fg(),
            right - left - count_w - Self::RESTORE_PAD,
        );

        let bar = Rect::from_min_size(
            pos2(left, py + Self::RESTORE_BAR_Y),
            vec2(right - left, Self::RESTORE_BAR_H),
        );
        painter.rect_filled(bar, CornerRadius::ZERO, theme::separator());
        let mut done = bar;
        done.set_width(bar.width() * restore.fraction());
        painter.rect_filled(done, CornerRadius::ZERO, theme::accent());
    }

    /// What the restore is doing now: loading the next plugin, or waiting
    /// for the catalog scan to find it.
    fn restore_line(&self, restore: &InstrumentRestore) -> String {
        if self.restore_waits_for_scan() {
            return "Waiting for the plugin scan…".to_owned();
        }
        match restore.next() {
            Some((_, want)) => format!("Loading {}", want.display_name),
            None => "Loading plugins".to_owned(),
        }
    }
}
