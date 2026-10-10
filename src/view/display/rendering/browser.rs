//! The browser side panel's painting: the heading and the tree rows (the
//! **Projects** and **Plugins** categories), on a layer of its own left of
//! the shifted canvas. The model and
//! input are `browser.rs`; see `030-ui-design.md` § Browser Panel.

use egui::{Id, LayerId, Order, Pos2, Shape};

use super::super::browser::{
    BROWSER_DISCLOSURE_W, BROWSER_INDENT_X, BROWSER_LIST_TOP, BROWSER_ROW_H, BROWSER_W,
    BrowserItem, browser_row_rect, browser_text_x,
};
use super::*;

/// Right inset of a plugin row's format label.
const FORMAT_RIGHT_X: f32 = 10.0;
/// Width of the current-folder / open-project marker bar.
const MARKER_W: f32 = 3.0;
/// Half the size of a folder's disclosure triangle.
const DISCLOSURE_HALF: f32 = 4.0;

/// A folder's disclosure triangle centred on `centre`: pointing right when
/// collapsed, down when expanded (Finder, Ableton). Painted rather than a
/// `▸`/`▾` glyph, which egui's proportional font doesn't have.
fn disclosure_triangle(centre: Pos2, expanded: bool) -> [Pos2; 3] {
    let h = DISCLOSURE_HALF;
    // Slightly narrower than tall, so it reads as a pointer, not a wedge.
    let w = h * 0.85;
    if expanded {
        [
            centre + vec2(-h, -w * 0.6),
            centre + vec2(h, -w * 0.6),
            centre + vec2(0.0, w),
        ]
    } else {
        [
            centre + vec2(-w * 0.6, -h),
            centre + vec2(w, 0.0),
            centre + vec2(-w * 0.6, h),
        ]
    }
}

impl Display {
    /// The layer the panel paints on: above the canvas's (background) layer,
    /// which carries the shift, so the panel itself stays put.
    pub(in crate::view::display) fn browser_layer() -> LayerId {
        LayerId::new(Order::Middle, Id::new("browser-panel"))
    }

    /// Paints the panel along the left of `window`.
    pub(super) fn draw_browser(&self, painter: &Painter, window: Rect) {
        let panel = Rect::from_min_max(window.min, pos2(window.min.x + BROWSER_W, window.max.y));
        let painter = painter.with_clip_rect(panel);
        painter.rect_filled(panel, CornerRadius::ZERO, theme::bg_panel());
        painter.vline(
            panel.max.x - 0.5,
            panel.y_range(),
            Stroke::new(1.0, theme::separator()),
        );

        let focused = self.key_focus == KeyFocus::Browser;
        let font = FontId::proportional(theme::FONT_SIZE_LABEL);
        painter.text(
            pos2(panel.min.x + 12.0, panel.min.y + theme::HEADER_H * 0.5),
            Align2::LEFT_CENTER,
            "BROWSER",
            font.clone(),
            accent_if(focused, theme::fg_dim()),
        );
        // With the keyboard: the panes' focus edge (`draw_pane_split`), at
        // the panes' top, so it reads as the same mark moving over.
        if focused {
            painter.hline(
                panel.min.x..=(panel.max.x - 1.0),
                panel.min.y + theme::HEADER_H + Self::HEADER_TIMELINE_GAP_Y,
                focus_edge_stroke(),
            );
        }

        let list = Rect::from_min_max(
            pos2(panel.min.x, panel.min.y + BROWSER_LIST_TOP),
            pos2(panel.max.x - 1.0, panel.max.y - theme::STATUS_H),
        );
        let painter = painter.with_clip_rect(list);
        let tree = &self.browser.tree;
        let rows = tree.rows();

        let current_folder = self.project.project_current_folder.as_deref();
        let current_name = self.project.project_current_name.as_deref();
        let selected = tree.selected();
        for (i, row) in rows.iter().skip(self.browser.scroll).enumerate() {
            let row_rect = browser_row_rect(panel.min, i);
            let top = row_rect.min.y;
            if top > list.max.y {
                break;
            }
            let mid_y = row_rect.center().y;
            let indent = f32::from(row.depth) * BROWSER_INDENT_X;
            let is_selected = selected == Some(&row.item);
            if is_selected {
                let alpha = if focused { 0.18 } else { 0.08 };
                painter.rect_filled(
                    row_rect,
                    CornerRadius::ZERO,
                    theme::accent().gamma_multiply(alpha),
                );
            }

            if let Some(expanded) = tree.item_expanded(&row.item) {
                let centre = pos2(row_rect.min.x + indent + BROWSER_DISCLOSURE_W * 0.5, mid_y);
                painter.add(Shape::convex_polygon(
                    disclosure_triangle(centre, expanded).to_vec(),
                    accent_if(is_selected && focused, theme::fg_dim()),
                    Stroke::NONE,
                ));
            }

            // The current folder and the open project get the region-coloured
            // marker and text.
            let (label, is_current) = match &row.item {
                BrowserItem::Category(category) => (category.label().to_owned(), false),
                BrowserItem::Folder(name) => (name.clone(), current_folder == Some(name.as_str())),
                BrowserItem::Project { folder, name } => (
                    name.clone(),
                    folder.as_deref() == current_folder && Some(name.as_str()) == current_name,
                ),
                BrowserItem::MidiFile { name, .. } => (format!("{name}.mid"), false),
                BrowserItem::Plugin(plugin) => {
                    // Right-aligned, so no text measurement is needed.
                    painter.text(
                        pos2(row_rect.max.x - FORMAT_RIGHT_X, mid_y),
                        Align2::RIGHT_CENTER,
                        plugin.format,
                        font.clone(),
                        theme::fg_dim(),
                    );
                    (plugin.name.clone(), false)
                }
                BrowserItem::Scanning => ("Scanning…".to_owned(), false),
            };
            if is_current {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(row_rect.min.x + 2.0, top + 3.0),
                        pos2(row_rect.min.x + 2.0 + MARKER_W, top + BROWSER_ROW_H - 3.0),
                    ),
                    CornerRadius::ZERO,
                    theme::region(),
                );
            }

            let (text, color) = if is_selected && tree.delete_armed {
                (format!("Delete {label}?  Enter / Esc"), theme::track_mute())
            } else if is_current {
                (label, theme::region())
            } else if matches!(
                row.item,
                BrowserItem::MidiFile { .. } | BrowserItem::Scanning | BrowserItem::Category(_)
            ) && !(is_selected && focused)
            {
                (label, theme::fg_dim())
            } else {
                (label, accent_if(is_selected && focused, theme::fg()))
            };
            painter.text(
                pos2(row_rect.min.x + browser_text_x(row.depth), mid_y),
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclosure_triangle_points_right_when_collapsed_and_down_when_expanded() {
        let centre = pos2(10.0, 10.0);
        let [_, tip, _] = disclosure_triangle(centre, false);
        assert!(tip.x > centre.x && tip.y == centre.y);
        let [_, _, tip] = disclosure_triangle(centre, true);
        assert!(tip.y > centre.y && tip.x == centre.x);
    }

    #[test]
    fn disclosure_triangle_fits_the_row() {
        for expanded in [false, true] {
            for p in disclosure_triangle(pos2(0.0, 0.0), expanded) {
                assert!(p.y.abs() < BROWSER_ROW_H * 0.5);
                assert!(p.x.abs() < BROWSER_DISCLOSURE_W * 0.5);
            }
        }
    }
}
