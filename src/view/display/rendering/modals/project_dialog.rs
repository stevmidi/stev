//! Draws the project dialogs (`project_dialog.rs`): Save As's name field and
//! the unsaved-changes prompt, each a modal frame centred on the canvas. The
//! prompt's button layout is shared with its click test
//! ([`unsaved_button_at`](Display::unsaved_button_at)).

use egui::{
    Align2, CornerRadius, FontId, Id, Margin, Painter, Pos2, Rect, Stroke, StrokeKind, TextEdit,
    Ui, pos2, vec2,
};

use crate::core::config::PROJECT_NAME_MAX_CHARS;
use crate::core::project::{ProjectAction, project_exists, project_name_from_input};
use crate::view::display::project_dialog::{ProjectDialog, SaveAs, UnsavedButton};
use crate::view::display::project_state::ProjectViewState;
use crate::view::display::track_rename::show_name_field;

use super::*;

/// Where a project dialog's parts sit on the canvas.
struct DialogLayout {
    /// The whole panel.
    panel: Rect,
    /// The text: Save As's name field, the prompt's question.
    body: Rect,
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
        let panel_h = Self::DIALOG_HEADER_H
            + Self::DIALOG_BODY_H
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
        let note = Rect::from_min_size(
            pos2(body.min.x, body.max.y),
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
            note,
            buttons,
        }
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
            Some(ProjectDialog::Unsaved { action }) => Self::draw_unsaved_prompt(
                ui.painter(),
                rect,
                action,
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

        let field_h = Self::FONT_LIST * 1.6;
        let field_rect = Rect::from_min_size(
            pos2(layout.body.min.x, layout.body.center().y - field_h * 0.5),
            vec2(layout.body.width(), field_h),
        );
        let field = TextEdit::singleline(&mut save_as.text)
            .char_limit(PROJECT_NAME_MAX_CHARS)
            .font(FontId::proportional(Self::FONT_LIST));
        show_name_field(
            ui,
            field,
            Id::new("project-save-as"),
            field_rect,
            Margin::symmetric(6, 2),
            &mut save_as.shown,
        );

        let folder = project.project_current_folder.as_deref();
        let note = match project_name_from_input(&save_as.text) {
            None => "Not a valid file name".to_owned(),
            Some(name)
                if project.project_current_name.as_deref() != Some(name.as_str())
                    && project_exists(folder, &name) =>
            {
                format!("Replaces the existing {name}")
            }
            Some(_) => format!("Saves into {}", folder.unwrap_or("Projects")),
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
            layout.note.right_center(),
            Align2::RIGHT_CENTER,
            "Enter save · Esc cancel",
            hint_font,
            theme::fg_dim(),
        );
    }

    /// The unsaved-changes prompt: the question, the key hints and the three
    /// buttons, Save (the default, Enter) in the accent colour.
    /// `name` is the open project's, `None` for one never saved.
    fn draw_unsaved_prompt(
        painter: &Painter,
        rect: Rect,
        action: &ProjectAction,
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

        let hint_font = FontId::proportional(Self::FONT_HINT);
        painter.text(
            layout.note.left_center(),
            Align2::LEFT_CENTER,
            "Enter save · D don't save · Esc cancel",
            hint_font.clone(),
            theme::fg_dim(),
        );

        for (button, button_rect) in UnsavedButton::ALL.into_iter().zip(layout.buttons) {
            let is_default = button == UnsavedButton::Save;
            painter.rect_filled(
                button_rect,
                CornerRadius::same(3),
                accent_if(is_default, theme::fg_dim()).gamma_multiply(0.15),
            );
            painter.rect_stroke(
                button_rect,
                CornerRadius::same(3),
                Stroke::new(1.0_f32, accent_if(is_default, theme::separator())),
                StrokeKind::Inside,
            );
            painter.text(
                button_rect.center(),
                Align2::CENTER_CENTER,
                button.label(),
                hint_font.clone(),
                accent_if(is_default, theme::fg()),
            );
        }
    }
}
