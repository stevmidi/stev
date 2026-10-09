//! The modal screens — the settings modal, the help overlay and the project
//! dialogs (Save As, the unsaved-changes prompt) — overlays `Display` owns, each drawn over the view
//! underneath. One file per modal; this file holds the shared modal-frame
//! chrome.

mod help;
mod project_dialog;
mod settings;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2,
};

use crate::view::display::rendering::output_menu::draw_truncated;

use super::*;

impl Display {
    // --- Platform-aware modal scaling ---
    /// Uniform scale applied to every modal font / spacing.
    pub(super) const M_SCALE: f32 = 1.4;

    /// Modal title font size.
    const FONT_TITLE: f32 = 15.0 * Self::M_SCALE;
    /// Modal list-item font size.
    const FONT_LIST: f32 = 14.0 * Self::M_SCALE;
    /// Modal hint / footer font size.
    const FONT_HINT: f32 = 12.0 * Self::M_SCALE;

    /// Paints a modal's frame — a `panel_w` × `panel_h` panel centred in
    /// `rect`, background, outline and `title` — and returns the panel's
    /// top-left corner.
    fn draw_modal_frame(
        painter: &Painter,
        rect: Rect,
        panel_w: f32,
        panel_h: f32,
        title: &str,
    ) -> (f32, f32) {
        let px = rect.min.x + (rect.width() - panel_w) * 0.5;
        let py = rect.min.y + (rect.height() - panel_h) * 0.5;

        Self::draw_modal_panel(
            painter,
            Rect::from_min_size(pos2(px, py), vec2(panel_w, panel_h)),
        );
        painter.text(
            pos2(px + 20.0 * Self::M_SCALE, py + 24.0 * Self::M_SCALE),
            Align2::LEFT_TOP,
            title,
            FontId::proportional(Self::FONT_TITLE),
            theme::accent(),
        );
        (px, py)
    }

    /// Paints `text` on one line left-aligned at `left_center`, cut with an
    /// ellipsis at `max_width` — every line of a modal's content.
    fn draw_modal_text(
        painter: &Painter,
        left_center: Pos2,
        text: &str,
        font_id: FontId,
        color: Color32,
        max_width: f32,
    ) {
        let format = TextFormat {
            font_id,
            color,
            ..Default::default()
        };
        draw_truncated(
            painter,
            left_center,
            LayoutJob::single_section(text.to_owned(), format),
            max_width,
        );
    }

    /// Paints a modal's panel — background and outline — at `panel`.
    fn draw_modal_panel(painter: &Painter, panel: Rect) {
        painter.rect_filled(panel, CornerRadius::ZERO, theme::bg_panel());
        painter.rect_stroke(
            panel,
            CornerRadius::ZERO,
            Self::modal_outline(),
            StrokeKind::Outside,
        );
    }

    /// The modal outline and divider stroke — the output menu's too.
    pub(super) fn modal_outline() -> Stroke {
        Stroke::new(1.0_f32, theme::separator())
    }
}
