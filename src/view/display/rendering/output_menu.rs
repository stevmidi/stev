//! Painting a track header's output chip and the output menu it opens
//! (`display/output_menu.rs` holds the layout and the clicks). See
//! `030-ui-design.md` § Arranger Track Header.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, StrokeKind, pos2};

use super::*;

/// Inset of a chip's or a menu row's text from its left edge.
const TEXT_INSET_X: f32 = 4.0;
/// The menu's text size.
const MENU_FONT_SIZE: f32 = 12.0;
/// Opacity of the accent wash behind the menu item under the pointer — the
/// browser's focused-row tint.
const HOVER_ALPHA: f32 = 0.18;
/// The chip's label on a MIDI track, by channel — static, so the chip
/// allocates nothing of its own each frame.
const CHANNEL_CHIP_LABELS: [&str; 16] = [
    "Ch 1", "Ch 2", "Ch 3", "Ch 4", "Ch 5", "Ch 6", "Ch 7", "Ch 8", "Ch 9", "Ch 10", "Ch 11",
    "Ch 12", "Ch 13", "Ch 14", "Ch 15", "Ch 16",
];
/// The channel grid's cell numbers, by channel.
const CHANNEL_CELL_LABELS: [&str; 16] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
];

/// Paints `job` on one line left-aligned at `left_center`, cut to
/// `max_width` with an ellipsis — plugin names run longer than a chip or a
/// menu row, and track names than the header's name row.
pub(super) fn draw_truncated(
    painter: &Painter,
    left_center: Pos2,
    mut job: LayoutJob,
    max_width: f32,
) {
    job.wrap = TextWrapping::truncate_at_width(max_width);
    let galley = painter.layout_job(job);
    let top_left = pos2(left_center.x, left_center.y - galley.size().y * 0.5);
    painter.galley(top_left, galley, Color32::PLACEHOLDER);
}

/// Washes `rect` (a menu item, or the arranger's `+` row, under the
/// pointer) with the accent.
pub(super) fn draw_hover_wash(painter: &Painter, rect: Rect) {
    painter.rect_filled(
        rect,
        CornerRadius::same(2),
        theme::accent().gamma_multiply(HOVER_ALPHA),
    );
}

impl Display {
    /// Paints `track_idx`'s output chip: `Ch 3` on a MIDI track, the plugin's
    /// name (cut short) on a plugin track — in italics when the plugin isn't
    /// loaded (not installed, or off macOS). Trough-filled like an off S/M
    /// button; the label brightens while its menu is open.
    pub(super) fn draw_output_chip(&self, painter: &Painter, track_idx: usize, chip: Rect) {
        let (label, italics) = match &self.tracks[track_idx].route {
            TrackRoute::MidiOut { channel } => (CHANNEL_CHIP_LABELS[usize::from(*channel)], false),
            TrackRoute::Instrument { name } => {
                (name.as_str(), !self.track_has_instrument(track_idx))
            }
        };
        let open = self.gesture.output_menu == Some(track_idx);
        painter.rect_filled(chip, CornerRadius::same(2), grid_seam_color());
        let format = TextFormat {
            font_id: FontId::proportional(11.0),
            color: if open { theme::fg() } else { theme::fg_dim() },
            italics,
            ..Default::default()
        };
        draw_truncated(
            painter,
            pos2(chip.min.x + TEXT_INSET_X, chip.center().y),
            LayoutJob::single_section(label.to_owned(), format),
            chip.width() - 2.0 * TEXT_INSET_X,
        );
    }

    /// Paints the open output menu, if any, over everything but the modals:
    /// on a plugin track the plugin's name and format and `Remove <plugin>`,
    /// then the MIDI channel grid with the track's current channel lit, and
    /// `Rename` and `Delete track` (not on the last track) last.
    pub(super) fn draw_output_menu(&mut self, painter: &Painter) {
        let Some((track_idx, layout)) = self.output_menu() else {
            return;
        };
        let hover = self
            .canvas_pointer()
            .and_then(|pointer| layout.hit_at(pointer));
        let font = FontId::proportional(MENU_FONT_SIZE);
        let format = |color: Color32| TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        };
        let text_width = layout.label.width() - 2.0 * TEXT_INSET_X;
        let row_text_at = |row: Rect| pos2(row.min.x + TEXT_INSET_X, row.center().y);

        painter.rect_filled(layout.panel, CornerRadius::same(3), theme::bg_panel());
        painter.rect_stroke(
            layout.panel,
            CornerRadius::same(3),
            Self::modal_outline(),
            StrokeKind::Outside,
        );

        let route = &self.tracks[track_idx].route;
        if let (TrackRoute::Instrument { name }, Some(title), Some(remove), Some(divider_y)) =
            (route, layout.title, layout.remove, layout.divider_y)
        {
            let mut title_job = LayoutJob::single_section(name.clone(), format(theme::fg()));
            title_job.append(
                &format!("  {}", self.loaded_plugin_format(track_idx)),
                0.0,
                format(theme::fg_dim()),
            );
            draw_truncated(painter, row_text_at(title), title_job, text_width);

            if hover == Some(OutputMenuHit::Remove) {
                draw_hover_wash(painter, remove);
            }
            let remove_job =
                LayoutJob::single_section(format!("Remove {name}"), format(theme::fg()));
            draw_truncated(painter, row_text_at(remove), remove_job, text_width);
            painter.hline(
                layout.panel.min.x..=layout.panel.max.x,
                divider_y,
                Self::modal_outline(),
            );
        }
        let current_channel = match route {
            TrackRoute::MidiOut { channel } => Some(*channel),
            TrackRoute::Instrument { .. } => None,
        };

        painter.text(
            row_text_at(layout.label),
            Align2::LEFT_CENTER,
            "MIDI channel",
            font.clone(),
            theme::fg_dim(),
        );
        for (channel, cell) in (0u8..).zip(layout.channels) {
            let cell = cell.shrink(1.0);
            let text_color = if current_channel == Some(channel) {
                painter.rect_filled(cell, CornerRadius::same(2), theme::accent());
                theme::bg()
            } else {
                if hover == Some(OutputMenuHit::Channel(channel)) {
                    draw_hover_wash(painter, cell);
                }
                theme::fg()
            };
            painter.text(
                cell.center(),
                Align2::CENTER_CENTER,
                CHANNEL_CELL_LABELS[usize::from(channel)],
                font.clone(),
                text_color,
            );
        }

        painter.hline(
            layout.panel.min.x..=layout.panel.max.x,
            layout.track_divider_y,
            Self::modal_outline(),
        );
        let track_items = [
            (Some(layout.rename), OutputMenuHit::Rename, "Rename"),
            (layout.delete, OutputMenuHit::DeleteTrack, "Delete track"),
        ];
        for (row, hit, label) in track_items {
            let Some(row) = row else {
                continue;
            };
            if hover == Some(hit) {
                draw_hover_wash(painter, row);
            }
            painter.text(
                row_text_at(row),
                Align2::LEFT_CENTER,
                label,
                font.clone(),
                theme::fg(),
            );
        }
    }

    /// The format of `track_idx`'s loaded plugin (`VST3`, `CLAP`) for the
    /// menu's title, or `not loaded` when the project names a plugin this
    /// machine can't load.
    fn loaded_plugin_format(&self, track_idx: usize) -> &'static str {
        #[cfg(target_os = "macos")]
        if let Some(loaded) = self.track_instrument(track_idx) {
            return loaded.entry.format.label();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = track_idx;
        "not loaded"
    }
}
