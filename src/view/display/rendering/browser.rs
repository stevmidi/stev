//! The browser side panel's painting: the tree rows (the **Projects**,
//! **Clips** and **Plugins** categories), on a layer of its own left of the
//! shifted canvas. The model and input are `browser.rs`; see
//! `030-ui-design.md` § Browser Panel.

use egui::{Id, LayerId, Order, Pos2, Shape, StrokeKind, Vec2, epaint::QuadraticBezierShape};

use super::super::browser::{
    BROWSER_DISCLOSURE_W, BROWSER_FONT_SIZE, BROWSER_ICON_W, BROWSER_INDENT_X, BROWSER_LIST_PAD_Y,
    BROWSER_ROW_H, BrowserItem, browser_icon_x, browser_row_rect, browser_text_x,
};
use super::*;

/// Right inset of a plugin row's format label.
const FORMAT_RIGHT_X: f32 = 10.0;
/// Width of the current-folder / open-project marker bar.
const MARKER_W: f32 = 3.0;
/// Half the size of a folder's disclosure triangle.
const DISCLOSURE_HALF: f32 = 4.0;

/// The panel toggle's icon: a window with a sidebar, as macOS's
/// `sidebar.left` (Zed, Obsidian), `TOGGLE_ICON_W` × `TOGGLE_ICON_H`.
const TOGGLE_ICON_W: f32 = 14.0;
/// See [`TOGGLE_ICON_W`].
const TOGGLE_ICON_H: f32 = 13.0;
/// Where the sidebar divider sits, as a fraction of the icon's width.
const TOGGLE_DIVIDER_AT: f32 = 0.4;
/// Side of the rounded square washed behind the icon under the pointer.
const TOGGLE_WASH_SIZE: f32 = 22.0;

/// Opacity of the `fg` wash behind the row under the pointer: fainter than
/// the unfocused selection's accent wash, so it never reads as one.
const HOVER_WASH_ALPHA: f32 = 0.04;

/// Side of the square a row's icon is drawn in.
const ICON_SIZE: f32 = 14.0;
// The icon fits its column with a gap before the text, and its row.
const _: () = assert!(ICON_SIZE < BROWSER_ICON_W && ICON_SIZE < BROWSER_ROW_H);
/// The icons' line width: the panel toggle's, so they read as one set.
const ICON_STROKE: f32 = TOGGLE_ICON_STROKE;
/// The document icon's lines of content.
const DOC_LINE_STROKE: f32 = 1.0;

/// What a row's icon shows: what kind of thing the row is, at a glance, as
/// Zed's project panel does. Monochrome, like the text it sits beside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserIcon {
    /// A project folder, drawn open while expanded.
    Folder {
        /// Whether the folder is expanded.
        open: bool,
    },
    /// A `.stev` project: a document.
    Project,
    /// A `.mid` clip: beamed eighth notes.
    Clip,
    /// An instrument plugin: a plug.
    Plugin,
}

impl BrowserIcon {
    /// The icon of `item`, given whether it is expanded (`None` for a row
    /// that can't be); `None` for a category or "Scanning…", which have none.
    fn of(item: &BrowserItem, expanded: Option<bool>) -> Option<Self> {
        match item {
            BrowserItem::Folder(_) => Some(BrowserIcon::Folder {
                open: expanded == Some(true),
            }),
            BrowserItem::Project { .. } => Some(BrowserIcon::Project),
            BrowserItem::Clip(_) => Some(BrowserIcon::Clip),
            BrowserItem::Plugin(_) => Some(BrowserIcon::Plugin),
            BrowserItem::Category(_) | BrowserItem::Scanning => None,
        }
    }

    /// The icon's shapes in `color`, in the `ICON_SIZE` square whose top-left
    /// corner is `origin`. Drawn on a 14-point grid; lines sit half their
    /// width in from the square's edge, so nothing pokes out of it.
    fn shapes(self, origin: Pos2, color: Color32) -> Vec<Shape> {
        let p = |x: f32, y: f32| origin + vec2(x, y);
        let stroke = Stroke::new(ICON_STROKE, color);
        match self {
            // Corners rounded by size: the outer ones most, the tab and the
            // fold less, and none where two of the icon's lines meet, which
            // would leave one line's end poking past the other's curve.
            BrowserIcon::Folder { open: false } => vec![Shape::closed_line(
                rounded(&[
                    (p(0.75, 2.0), 1.5),
                    (p(5.25, 2.0), 0.75),
                    (p(6.75, 3.75), 0.75),
                    (p(13.25, 3.75), 1.5),
                    (p(13.25, 12.0), 1.5),
                    (p(0.75, 12.0), 1.5),
                ]),
                stroke,
            )],
            // The back of the folder, and its front flap tipped forward.
            BrowserIcon::Folder { open: true } => vec![
                Shape::line(
                    rounded_open(&[
                        (p(11.5, 6.0), 0.0),
                        (p(11.5, 3.75), 1.0),
                        (p(6.75, 3.75), 0.75),
                        (p(5.25, 2.0), 0.75),
                        (p(0.75, 2.0), 1.5),
                        (p(0.75, 12.0), 0.0),
                    ]),
                    stroke,
                ),
                Shape::closed_line(
                    rounded(&[
                        (p(0.75, 12.0), 0.0),
                        (p(3.25, 6.0), 0.75),
                        (p(13.25, 6.0), 0.75),
                        (p(10.75, 12.0), 1.0),
                    ]),
                    stroke,
                ),
            ],
            // A page with its top-right corner folded and three lines of
            // content, the last one short: the usual document sign, outlined
            // like the folder it sits in. The lines are thinner than the
            // outline, so they don't crowd.
            BrowserIcon::Project => {
                let line = Stroke::new(DOC_LINE_STROKE, color);
                vec![
                    Shape::closed_line(
                        rounded(&[
                            (p(1.75, 1.0), 1.5),
                            (p(8.75, 1.0), 0.0),
                            (p(12.25, 4.5), 0.0),
                            (p(12.25, 13.0), 1.5),
                            (p(1.75, 13.0), 1.5),
                        ]),
                        stroke,
                    ),
                    Shape::line(
                        rounded_open(&[
                            (p(8.75, 1.0), 0.0),
                            (p(8.75, 4.5), 0.75),
                            (p(12.25, 4.5), 0.0),
                        ]),
                        stroke,
                    ),
                    Shape::line_segment([p(4.25, 6.5), p(9.75, 6.5)], line),
                    Shape::line_segment([p(4.25, 8.5), p(9.75, 8.5)], line),
                    Shape::line_segment([p(4.25, 10.5), p(8.25, 10.5)], line),
                ]
            }
            // Two beamed eighth notes, the usual sign for a MIDI file.
            BrowserIcon::Clip => vec![
                Shape::circle_filled(p(3.5, 10.75), 2.25, color),
                Shape::circle_filled(p(10.5, 9.5), 2.25, color),
                Shape::line_segment([p(5.0, 3.5), p(5.0, 10.5)], stroke),
                Shape::line_segment([p(12.0, 2.0), p(12.0, 9.25)], stroke),
                Shape::convex_polygon(
                    vec![p(4.25, 2.25), p(12.75, 0.75), p(12.75, 2.75), p(4.25, 4.25)],
                    color,
                    Stroke::NONE,
                ),
            ],
            // A plug, "plug-in" taken literally, lying on its side: the cord
            // in from the left, a body widening to two prongs pointing right.
            // Solid, like the clip's notes.
            BrowserIcon::Plugin => vec![
                Shape::line_segment([p(9.5, 5.0), p(13.25, 5.0)], stroke),
                Shape::line_segment([p(9.5, 9.0), p(13.25, 9.0)], stroke),
                Shape::convex_polygon(
                    vec![
                        p(10.0, 2.75),
                        p(10.0, 11.25),
                        p(7.0, 11.25),
                        p(4.25, 8.75),
                        p(4.25, 5.25),
                        p(7.0, 2.75),
                    ],
                    color,
                    Stroke::NONE,
                ),
                Shape::line_segment([p(0.75, 7.0), p(4.5, 7.0)], stroke),
            ],
        }
    }
}

/// How many segments draw each rounded corner of an icon's outline.
const CORNER_STEPS: usize = 4;

/// The closed outline through `corners`, each `(point, radius)` rounded by
/// its radius (`0`: left sharp). See [`round_corners`].
fn rounded(corners: &[(Pos2, f32)]) -> Vec<Pos2> {
    round_corners(corners, true)
}

/// The open line through `corners`, as [`rounded`]; its two ends stay as
/// they are.
fn rounded_open(corners: &[(Pos2, f32)]) -> Vec<Pos2> {
    round_corners(corners, false)
}

/// The path through `corners` with each corner whose radius is above `0`
/// replaced by a curve: a quadratic Bézier from where a circle of that radius
/// would meet the incoming edge to where it meets the outgoing one, the
/// corner as its control point. The curve stays inside the corner's
/// triangle, so the icon's bounds never grow. A radius too big for a short
/// edge is cut to fit half of it, so neighbouring curves never overlap.
fn round_corners(corners: &[(Pos2, f32)], closed: bool) -> Vec<Pos2> {
    let n = corners.len();
    let mut path = Vec::with_capacity(n * (CORNER_STEPS + 1));
    for (i, &(corner, radius)) in corners.iter().enumerate() {
        let is_end = !closed && (i == 0 || i == n - 1);
        if radius <= 0.0 || is_end {
            path.push(corner);
            continue;
        }
        let to_prev = corners[(i + n - 1) % n].0 - corner;
        let to_next = corners[(i + 1) % n].0 - corner;
        let half_angle = to_prev
            .normalized()
            .dot(to_next.normalized())
            .clamp(-1.0, 1.0)
            .acos()
            * 0.5;
        let reach = (radius / half_angle.tan())
            .min(to_prev.length() * 0.5)
            .min(to_next.length() * 0.5);
        let start = corner + to_prev.normalized() * reach;
        let end = corner + to_next.normalized() * reach;
        let curve = QuadraticBezierShape::from_points_stroke(
            [start, corner, end],
            false,
            Color32::TRANSPARENT,
            Stroke::NONE,
        );
        path.extend((0..=CORNER_STEPS).map(|step| curve.sample(step as f32 / CORNER_STEPS as f32)));
    }
    path
}

/// A folder's disclosure triangle centred on `centre`: pointing right when
/// collapsed, down when expanded (Finder, Ableton). Painted rather than a
/// `▸`/`▾` glyph, which the UI font doesn't have.
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

    /// Paints the footer's panel toggle on `painter` (the footer's): the
    /// sidebar outline in `fg_dim`, its sidebar filled while the panel is
    /// open; under the pointer, the icon in `fg` on a faint neutral square.
    /// Neutral throughout — accent means the keyboard here.
    pub(super) fn draw_browser_toggle(&self, painter: &Painter, toggle: Rect) {
        let open = self.browser.visible;
        let hover = self.is_pointer_on_browser_toggle();

        if hover {
            painter.rect_filled(
                Rect::from_center_size(toggle.center(), Vec2::splat(TOGGLE_WASH_SIZE)),
                CornerRadius::same(4),
                theme::fg().gamma_multiply(0.07),
            );
        }

        let color = toggle_icon_color(false, hover);
        // Whole points, so the inside stroke lands on whole pixels.
        let size = vec2(TOGGLE_ICON_W, TOGGLE_ICON_H);
        let outline = Rect::from_min_size((toggle.center() - size * 0.5).round(), size);
        // `seam_y` snaps an x as well as a y.
        let divider_x = seam_y(
            outline.min.x + TOGGLE_ICON_W * TOGGLE_DIVIDER_AT,
            TOGGLE_ICON_STROKE,
            painter.pixels_per_point(),
        );
        if open {
            painter.rect_filled(
                outline.with_max_x(divider_x),
                CornerRadius {
                    nw: TOGGLE_ICON_RADIUS,
                    sw: TOGGLE_ICON_RADIUS,
                    ne: 0,
                    se: 0,
                },
                color,
            );
        }
        let stroke = Stroke::new(TOGGLE_ICON_STROKE, color);
        painter.rect_stroke(
            outline,
            CornerRadius::same(TOGGLE_ICON_RADIUS),
            stroke,
            StrokeKind::Inside,
        );
        painter.vline(divider_x, outline.y_range(), stroke);
    }

    /// Paints the panel along the left of `window`, down to the footer,
    /// which runs under it (`draw_status_bar`).
    pub(super) fn draw_browser(&self, painter: &Painter, window: Rect) {
        let width = self.browser.width;
        let panel = Rect::from_min_max(window.min, pos2(window.min.x + width, self.footer_top()));
        let painter = painter.with_clip_rect(panel);
        painter.rect_filled(panel, CornerRadius::ZERO, theme::bg_panel());
        // Under the pointer or dragged, the separator shows its edge can be
        // dragged.
        let edge = if self.is_pointer_on_browser_edge() {
            theme::fg_dim()
        } else {
            theme::separator()
        };
        painter.vline(panel.max.x - 0.5, panel.y_range(), Stroke::new(1.0, edge));

        let focused = self.key_owner() == KeyFocus::Browser;
        let font = FontId::proportional(BROWSER_FONT_SIZE);
        // With the keyboard: the panes' focus edge (`draw_pane_split`), along
        // the panel's top — one physical pixel down, so the window's edge
        // doesn't clip the stroke's anti-aliased fringe and thin it.
        if focused {
            let stroke = focus_edge_stroke();
            painter.hline(
                panel.min.x..=(panel.max.x - 1.0),
                panel.min.y + stroke.width * 0.5 + 1.0 / painter.pixels_per_point(),
                stroke,
            );
        }

        let list = Rect::from_min_max(
            pos2(panel.min.x, panel.min.y + BROWSER_LIST_PAD_Y),
            pos2(panel.max.x - 1.0, self.browser_list_bottom()),
        );
        let painter = painter.with_clip_rect(list);
        let tree = &self.browser.tree;
        let rows = tree.rows();

        let current_folder = self.project.project_current_folder.as_deref();
        let current_name = self.project.project_current_name.as_deref();
        let selected = tree.selected();
        let scroll = self.browser.scroll;
        let hovered = self.hovered_browser_row();
        let first = (scroll / BROWSER_ROW_H) as usize;
        for (i, row) in rows.iter().enumerate().skip(first) {
            let row_rect = browser_row_rect(panel.min, width, i, scroll);
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
            } else if hovered == Some(i) {
                // Neutral, as the panel toggle's hover: accent means the
                // keyboard.
                painter.rect_filled(
                    row_rect,
                    CornerRadius::ZERO,
                    theme::fg().gamma_multiply(HOVER_WASH_ALPHA),
                );
            }

            // The triangle's and the icon's colour: dim, accent on the
            // focused selection.
            let mark = accent_if(is_selected && focused, theme::fg_dim());
            let expanded = tree.item_expanded(&row.item);
            if let Some(expanded) = expanded {
                let centre = pos2(row_rect.min.x + indent + BROWSER_DISCLOSURE_W * 0.5, mid_y);
                painter.add(Shape::convex_polygon(
                    disclosure_triangle(centre, expanded).to_vec(),
                    mark,
                    Stroke::NONE,
                ));
            }

            // The current folder and the open project get the region-coloured
            // marker, icon and text.
            let (label, is_current) = match &row.item {
                BrowserItem::Category(category) => (category.label().to_owned(), false),
                BrowserItem::Folder(name) => (name.clone(), current_folder == Some(name.as_str())),
                BrowserItem::Project { folder, name } => (
                    name.clone(),
                    folder.as_deref() == current_folder && Some(name.as_str()) == current_name,
                ),
                BrowserItem::Clip(name) => (format!("{name}.mid"), false),
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
            if let Some(icon) = BrowserIcon::of(&row.item, expanded) {
                // Whole points, so the lines land on whole pixels on Retina.
                let origin = pos2(
                    row_rect.min.x + browser_icon_x(row.depth),
                    mid_y - ICON_SIZE * 0.5,
                )
                .round();
                let color = if is_current { theme::region() } else { mark };
                painter.extend(icon.shapes(origin, color));
            }
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
                BrowserItem::Clip(_) | BrowserItem::Scanning | BrowserItem::Category(_)
            ) && !(is_selected && focused)
            {
                (label, theme::fg_dim())
            } else {
                (label, accent_if(is_selected && focused, theme::fg()))
            };
            painter.text(
                pos2(row_rect.min.x + browser_text_x(row), mid_y),
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
    use super::super::super::browser::BrowserCategory;
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
    fn every_icon_stays_inside_its_square_and_column() {
        let origin = pos2(0.0, 0.0);
        let square = Rect::from_min_size(origin, Vec2::splat(ICON_SIZE));
        for icon in [
            BrowserIcon::Folder { open: false },
            BrowserIcon::Folder { open: true },
            BrowserIcon::Project,
            BrowserIcon::Clip,
            BrowserIcon::Plugin,
        ] {
            for shape in icon.shapes(origin, Color32::WHITE) {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    square.expand(0.01).contains_rect(bounds),
                    "{icon:?} pokes out: {bounds:?}"
                );
            }
        }
    }

    #[test]
    fn round_corners_leaves_sharp_corners_and_open_ends_alone() {
        let square = [
            (pos2(0.0, 0.0), 0.0),
            (pos2(10.0, 0.0), 0.0),
            (pos2(10.0, 10.0), 0.0),
        ];
        let points: Vec<Pos2> = square.iter().map(|&(point, _)| point).collect();
        assert_eq!(rounded(&square), points);
        // An open line's ends never round, whatever their radius.
        let ends = [
            (pos2(0.0, 0.0), 2.0),
            (pos2(10.0, 0.0), 0.0),
            (pos2(10.0, 10.0), 2.0),
        ];
        assert_eq!(rounded_open(&ends), points);
    }

    #[test]
    fn round_corners_cuts_a_corner_by_its_radius() {
        // A right angle with radius 2: the curve starts 2 along each edge
        // (tan 45° = 1) and never reaches the corner itself.
        let path = rounded_open(&[
            (pos2(0.0, 0.0), 0.0),
            (pos2(10.0, 0.0), 2.0),
            (pos2(10.0, 10.0), 0.0),
        ]);
        assert_eq!(path.len(), 2 + CORNER_STEPS + 1);
        assert_eq!(path[1], pos2(8.0, 0.0));
        assert_eq!(path[path.len() - 2], pos2(10.0, 2.0));
        assert!(!path.contains(&pos2(10.0, 0.0)));
    }

    #[test]
    fn round_corners_fits_a_radius_to_a_short_edge() {
        // Radius 5 on a corner whose edges are 2 long: cut to half an edge.
        let path = rounded_open(&[
            (pos2(0.0, 0.0), 0.0),
            (pos2(2.0, 0.0), 5.0),
            (pos2(2.0, 2.0), 0.0),
        ]);
        assert_eq!(path[1], pos2(1.0, 0.0));
        assert_eq!(path[path.len() - 2], pos2(2.0, 1.0));
    }

    #[test]
    fn icons_follow_the_row_kind() {
        let folder = BrowserItem::Folder("Songs".to_owned());
        assert_eq!(
            BrowserIcon::of(&folder, Some(true)),
            Some(BrowserIcon::Folder { open: true })
        );
        assert_eq!(
            BrowserIcon::of(&folder, Some(false)),
            Some(BrowserIcon::Folder { open: false })
        );
        let project = BrowserItem::Project {
            folder: None,
            name: "Sketch".to_owned(),
        };
        assert_eq!(BrowserIcon::of(&project, None), Some(BrowserIcon::Project));
        let clip = BrowserItem::Clip("Riff".to_owned());
        assert_eq!(BrowserIcon::of(&clip, None), Some(BrowserIcon::Clip));
        assert_eq!(BrowserIcon::of(&BrowserItem::Scanning, None), None);
        let category = BrowserItem::Category(BrowserCategory::Projects);
        assert_eq!(BrowserIcon::of(&category, Some(true)), None);
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
