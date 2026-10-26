//! The modal screens — the settings modal, the plugin-restore overlay (macOS) and the project dialogs (Save As, the
//! unsaved-changes prompt) — overlays `Display` owns, each drawn over the view
//! underneath. One file per modal; this file holds the shared modal-frame
//! chrome.

#[cfg(target_os = "macos")]
mod instrument_restore;
mod project_dialog;
mod settings;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2,
};

use crate::view::display::rendering::output_menu::{draw_hover_wash, draw_truncated};

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
    /// Height of a modal's text field (Save As's name, the Keyboard tab's
    /// search), in the list font: its line with room above and below for
    /// the caret, which egui draws as the line plus 3 points.
    const FIELD_H: f32 = 2.0 * Self::FONT_LIST;

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

    /// Paints a modal's button: a faint fill and an outline, `accent` while
    /// `is_selected`, with `label` centred in the hint font in `color`.
    fn draw_modal_button(
        painter: &Painter,
        rect: Rect,
        label: &str,
        is_selected: bool,
        color: Color32,
    ) {
        painter.rect_filled(
            rect,
            CornerRadius::same(3),
            accent_if(is_selected, theme::fg_dim()).gamma_multiply(0.15),
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0_f32, accent_if(is_selected, theme::separator())),
            StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(Self::FONT_HINT),
            color,
        );
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
        control_edge(false)
    }
}
