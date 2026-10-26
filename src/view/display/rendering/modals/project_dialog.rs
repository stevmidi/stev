//! Draws the project dialogs (`project_dialog.rs`): Save As's name field and
//! the unsaved-changes prompt, each a modal frame centred on the canvas. The
//! prompt's button layout is shared with its click test
//! ([`unsaved_button_at`](Display::unsaved_button_at)).

use egui::{
    Align2, CornerRadius, FontId, Id, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Ui, pos2,
    vec2,
};

use crate::core::config::PROJECT_NAME_MAX_CHARS;
use crate::core::project::{ProjectAction, file_stem, project_exists, project_name_from_input};
use crate::view::display::project_dialog::{
    ProjectDialog, SaveAs, SaveAsHit, UnsavedButton, list_window,
};
use crate::view::display::project_state::ProjectViewState;
use crate::view::display::text_field::{FieldStyle, MODAL_MARGIN};

use super::*;

/// Where a project dialog's parts sit on the canvas.
struct DialogLayout {
    /// The whole panel.
    panel: Rect,
    /// The text: Save As's name field, the prompt's question.
    body: Rect,
    /// Save As's label for the destination row (empty on the prompt).
    folder_label: Rect,
    /// The dropdown's box, where the chosen folder shows.
    folder_box: Rect,
    /// The line under it: Save As's note, the prompt's key hints.
    note: Rect,
    /// The prompt's buttons, left to right as in [`UnsavedButton::ALL`].
    buttons: [Rect; 3],
}

impl Display {
    /// Panel width.
    const DIALOG_W: f32 = 520.0 * Self::M_SCALE;
    /// Title band height, as the other modals.
    const DIALOG_HEADER_H: f32 = 44.0 * Self::M_SCALE;
    /// Inset of everything from the panel's edges.
    const DIALOG_PAD: f32 = 20.0 * Self::M_SCALE;
    /// The text's height: the name field, or the prompt's question (two
    /// lines at most).
    const DIALOG_BODY_H: f32 = 44.0 * Self::M_SCALE;
    /// Height of Save As's destination row.
    const DIALOG_FOLDER_H: f32 = 34.0 * Self::M_SCALE;
    /// Width of the row's "Save in" label.
    const DIALOG_FOLDER_LABEL_W: f32 = 84.0 * Self::M_SCALE;
    /// Inset of the dropdown's text from its box and rows.
    const DIALOG_BOX_PAD: f32 = 8.0 * Self::M_SCALE;
    /// Height of one entry of the dropdown's list.
    const DIALOG_LIST_ROW_H: f32 = 26.0 * Self::M_SCALE;
    /// The note line's height.
    const DIALOG_NOTE_H: f32 = 22.0 * Self::M_SCALE;
    /// A prompt button's size.
    const DIALOG_BUTTON: [f32; 2] = [104.0 * Self::M_SCALE, 28.0 * Self::M_SCALE];
    /// Space between the prompt's buttons.
    const DIALOG_BUTTON_GAP: f32 = 8.0 * Self::M_SCALE;

    /// The dialog's layout in the canvas `rect`: the prompt has a button row
    /// under its note, Save As does not.
    fn dialog_layout(rect: Rect, with_buttons: bool) -> DialogLayout {
        let [button_w, button_h] = Self::DIALOG_BUTTON;
        let buttons_h = if with_buttons {
            button_h + Self::DIALOG_PAD * 0.5
        } else {
            0.0
        };
        let folder_h = if with_buttons {
            0.0
        } else {
            Self::DIALOG_FOLDER_H
        };
        let panel_h = Self::DIALOG_HEADER_H
            + Self::DIALOG_BODY_H
            + folder_h
            + Self::DIALOG_NOTE_H
            + buttons_h
            + Self::DIALOG_PAD * 0.75;
        let panel = Rect::from_center_size(rect.center(), vec2(Self::DIALOG_W, panel_h));
        let inner_w = Self::DIALOG_W - 2.0 * Self::DIALOG_PAD;
        let body = Rect::from_min_size(
            pos2(
                panel.min.x + Self::DIALOG_PAD,
                panel.min.y + Self::DIALOG_HEADER_H,
            ),
            vec2(inner_w, Self::DIALOG_BODY_H),
        );
        let folder_row = Rect::from_min_size(pos2(body.min.x, body.max.y), vec2(inner_w, folder_h));
        let folder_label = Rect::from_min_size(
            folder_row.min,
            vec2(Self::DIALOG_FOLDER_LABEL_W.min(inner_w), folder_h),
        );
        let folder_box = Rect::from_min_max(
            pos2(folder_label.max.x, folder_row.min.y + 2.0),
            pos2(folder_row.max.x, folder_row.max.y - 2.0),
        );
        let note = Rect::from_min_size(
            pos2(body.min.x, folder_row.max.y),
            vec2(inner_w, Self::DIALOG_NOTE_H),
        );
        let buttons_top = note.max.y + Self::DIALOG_PAD * 0.25;
        let buttons = [2.0, 1.0, 0.0].map(|from_right: f32| {
            let right =
                panel.max.x - Self::DIALOG_PAD - from_right * (button_w + Self::DIALOG_BUTTON_GAP);
            Rect::from_min_size(
                pos2(right - button_w, buttons_top),
                vec2(button_w, button_h),
            )
        });
        DialogLayout {
            panel,
            body,
            folder_label,
            folder_box,
            note,
            buttons,
        }
    }

    /// The open dropdown list: `rows` entries stacked under the box.
    fn folder_list_rect(folder_box: Rect, rows: usize) -> Rect {
        Rect::from_min_size(
            folder_box.left_bottom(),
            vec2(folder_box.width(), rows as f32 * Self::DIALOG_LIST_ROW_H),
        )
    }

    /// The `slot`th shown row of the list `list`.
    fn folder_list_row(list: Rect, slot: usize) -> Rect {
        Rect::from_min_size(
            pos2(
                list.min.x,
                list.min.y + slot as f32 * Self::DIALOG_LIST_ROW_H,
            ),
            vec2(list.width(), Self::DIALOG_LIST_ROW_H),
        )
    }

    /// What a press at canvas point `pos` lands on in the open Save As: a row
    /// of the open folder list, or the dropdown's box.
    pub(in crate::view::display) fn save_as_hit_at(&self, pos: Pos2) -> Option<SaveAsHit> {
        let Some(ProjectDialog::SaveAs(save_as)) = &self.project_dialog else {
            return None;
        };
        let layout = Self::dialog_layout(self.render.canvas_rect, false);
        if let Some(highlight) = save_as.list_highlight() {
            let window = list_window(highlight, save_as.folder_count());
            let list = Self::folder_list_rect(layout.folder_box, window.len());
            if let Some(slot) =
                (0..window.len()).find(|&slot| Self::folder_list_row(list, slot).contains(pos))
            {
                return Some(SaveAsHit::Row(window.start + slot));
            }
        }
        layout.folder_box.contains(pos).then_some(SaveAsHit::Box)
    }

    /// The unsaved-changes prompt's button under canvas point `pos`, if any.
    pub(in crate::view::display) fn unsaved_button_at(&self, pos: Pos2) -> Option<UnsavedButton> {
        let layout = Self::dialog_layout(self.render.canvas_rect, true);
        UnsavedButton::ALL
            .into_iter()
            .zip(layout.buttons)
            .find_map(|(button, rect)| rect.contains(pos).then_some(button))
    }

    /// Shows the open project dialog, if any, over the canvas `rect`, in
    /// the canvas `ui`.
    pub(in crate::view::display) fn show_project_dialog(&mut self, ui: &mut Ui, rect: Rect) {
        match &mut self.project_dialog {
            Some(ProjectDialog::SaveAs(save_as)) => {
                Self::show_save_as(ui, rect, save_as, &self.project)
            }
            Some(ProjectDialog::Unsaved { action, selected }) => Self::draw_unsaved_prompt(
                ui.painter(),
                rect,
                action,
                *selected,
                self.project.project_current_name.as_deref(),
            ),
            None => {}
        }
    }

    /// Save As: the name field, and a note under it — why the name can't be
    /// saved, that it replaces another project, or where it goes in
    /// `project`'s current folder. Its first frame takes the egui focus and
    /// selects the text.
    fn show_save_as(ui: &mut Ui, rect: Rect, save_as: &mut SaveAs, project: &ProjectViewState) {
        let layout = Self::dialog_layout(rect, false);
        Self::draw_modal_frame(
            ui.painter(),
            rect,
            layout.panel.width(),
            layout.panel.height(),
            "SAVE AS",
        );

        let field_rect = Rect::from_min_size(
            pos2(
                layout.body.min.x,
                layout.body.center().y - Self::FIELD_H * 0.5,
            ),
            vec2(layout.body.width(), Self::FIELD_H),
        );
        let style = FieldStyle::new(FontId::proportional(Self::FONT_LIST), MODAL_MARGIN)
            .max_chars(PROJECT_NAME_MAX_CHARS);
        save_as
            .field
            .show(ui, Id::new("project-save-as"), field_rect, style);
        let hover = ui.input(|i| i.pointer.hover_pos());

        let folder = save_as.folder();
        let is_open_project = |name: &str| {
            project.project_current_folder.as_deref() == folder
                && project.project_current_name.as_deref() == Some(name)
        };
        let note = match project_name_from_input(&save_as.field.text) {
            None => "Not a valid file name".to_owned(),
            Some(name) if !is_open_project(&name) && project_exists(folder, &name) => {
                format!("Replaces the existing {name}")
            }
            Some(_) => String::new(),
        };
        let painter = ui.painter();
        let hint_font = FontId::proportional(Self::FONT_HINT);
        painter.text(
            layout.note.left_center(),
            Align2::LEFT_CENTER,
            note,
            hint_font.clone(),
            theme::fg_dim(),
        );
        painter.text(
            layout.folder_label.left_center(),
            Align2::LEFT_CENTER,
            "Save in",
            hint_font.clone(),
            theme::fg_dim(),
        );
        Self::draw_folder_dropdown(painter, layout.folder_box, save_as, hover);
        painter.text(
            layout.note.right_center(),
            Align2::RIGHT_CENTER,
            "Enter save · Esc cancel",
            hint_font,
            theme::fg_dim(),
        );
    }

    /// The folder dropdown: the box with the chosen folder and a triangle,
    /// and under it, while open, the list: the highlighted row in the accent
    /// colour, the row under `hover` washed, the chosen folder's name bright.
    fn draw_folder_dropdown(
        painter: &Painter,
        folder_box: Rect,
        save_as: &SaveAs,
        hover: Option<Pos2>,
    ) {
        let font = FontId::proportional(Self::FONT_LIST);
        let radius = CornerRadius::same(3);
        let outline = Stroke::new(1.0, Self::modal_outline().color);
        let label = |text: &str, color: Color32| {
            LayoutJob::single_section(
                text.to_owned(),
                TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                },
            )
        };
        painter.rect_filled(folder_box, radius, theme::bg());
        painter.rect_stroke(folder_box, radius, outline, StrokeKind::Inside);
        draw_truncated(
            painter,
            pos2(
                folder_box.min.x + Self::DIALOG_BOX_PAD,
                folder_box.center().y,
            ),
            label(save_as.folder_label(save_as.folder_idx()), theme::fg()),
            folder_box.width() - 3.0 * Self::DIALOG_BOX_PAD - Self::FONT_LIST,
        );
        let tip = pos2(
            folder_box.max.x - Self::DIALOG_BOX_PAD - 5.0,
            folder_box.center().y + 2.5,
        );
        painter.add(Shape::convex_polygon(
            vec![tip + vec2(-5.0, -5.0), tip + vec2(5.0, -5.0), tip],
            theme::fg_dim(),
            Stroke::NONE,
        ));

        let Some(highlight) = save_as.list_highlight() else {
            return;
        };
        let window = list_window(highlight, save_as.folder_count());
        let list = Self::folder_list_rect(folder_box, window.len());
        painter.rect_filled(list, radius, theme::bg_panel());
        painter.rect_stroke(list, radius, outline, StrokeKind::Outside);
        for (slot, idx) in window.enumerate() {
            let row = Self::folder_list_row(list, slot);
            let color = if idx == highlight {
                painter.rect_filled(row.shrink(1.0), radius, theme::accent());
                theme::bg()
            } else {
                if hover.is_some_and(|p| row.contains(p)) {
                    draw_hover_wash(painter, row.shrink(1.0));
                }
                if idx == save_as.folder_idx() {
                    theme::fg()
                } else {
                    theme::fg_dim()
                }
            };
            draw_truncated(
                painter,
                pos2(row.min.x + Self::DIALOG_BOX_PAD, row.center().y),
                label(save_as.folder_label(idx), color),
                row.width() - 2.0 * Self::DIALOG_BOX_PAD,
            );
        }
    }

    /// The unsaved-changes prompt: the question, the key hints and the three
    /// buttons, the `selected` one (Enter's) in the accent colour.
    /// `name` is the open project's, `None` for one never saved.
    fn draw_unsaved_prompt(
        painter: &Painter,
        rect: Rect,
        action: &ProjectAction,
        selected: UnsavedButton,
        name: Option<&str>,
    ) {
        let layout = Self::dialog_layout(rect, true);
        Self::draw_modal_frame(
            painter,
            rect,
            layout.panel.width(),
            layout.panel.height(),
            "UNSAVED CHANGES",
        );

        let before = match action {
            ProjectAction::New => "before starting a new project".to_owned(),
            ProjectAction::Open { filename, .. } => format!("before opening {filename}"),
            ProjectAction::OpenFile { path } => format!("before opening {}", file_stem(path)),
            ProjectAction::Quit => "before quitting".to_owned(),
        };
        let question = match name {
            Some(name) => format!("Save the changes to {name} {before}?"),
            None => format!("Save this new project {before}?"),
        };
        let galley = painter.layout(
            question,
            FontId::proportional(Self::FONT_LIST),
            theme::fg(),
            layout.body.width(),
        );
        let text_pos = pos2(
            layout.body.min.x,
            layout.body.center().y - galley.size().y * 0.5,
        );
        painter.galley(text_pos, galley, theme::fg());

        painter.text(
            layout.note.left_center(),
            Align2::LEFT_CENTER,
            "←→ choose · Enter press · D don't save · Esc cancel",
            FontId::proportional(Self::FONT_HINT),
            theme::fg_dim(),
        );

        for (button, button_rect) in UnsavedButton::ALL.into_iter().zip(layout.buttons) {
            let is_selected = button == selected;
            let color = accent_if(is_selected, theme::fg());
            Self::draw_modal_button(painter, button_rect, button.label(), is_selected, color);
        }
    }
}
