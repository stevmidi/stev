//! The knob panel's keyboard focus (`KeyFocus::KnobPanel`,
//! `400-parameter-knobs.md` decision 14): a click or right-click on a knob
//! selects it and the panel takes the keyboard, as a click on a track
//! header gives the header column it — clip open or not. With a clip open
//! the click also shows the knob's lane under the piano roll. `Delete`
//! removes the selected knob's mapping; `Esc` leaves; a pane command hands
//! the keyboard back to the pane and acts there
//! ([`Action::leaves_knob_panel`]). Clearing a knob's recorded moves is the
//! lane's (`⌘A` or a span, then `Delete`), never the panel's.

use egui::Key;

use crate::core::input_event::InputEvent;
use crate::core::keymap::Action;
use crate::models::knobs::ParamId;

use super::super::status_message::StatusMessage;
use super::*;

/// What a key does while the knob panel has the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum KnobPanelKey {
    /// Remove the selected knob's mapping.
    Unassign,
    /// Give the keyboard back to the pane.
    Leave,
    /// Give the keyboard back to the pane and let the key act there.
    HandBack,
}

/// The knob panel's keymap, by the action the keymap resolves the press to
/// (`action`): Delete (`Delete`/`⌫`, whatever it is bound to, in either
/// pane) unassigns the knob; any other pane command hands the keyboard back
/// first. Esc, not a command, leaves. `None` — every other key, the
/// transport, the arrows and `W` among them — falls through to the pane as
/// usual.
fn knob_panel_key(key: Key, action: Option<Action>) -> Option<KnobPanelKey> {
    match (key, action) {
        (Key::Escape, _) => Some(KnobPanelKey::Leave),
        (_, Some(Action::DeleteNotes | Action::DeleteSelection)) => Some(KnobPanelKey::Unassign),
        (_, Some(action)) if action.leaves_knob_panel() => Some(KnobPanelKey::HandBack),
        _ => None,
    }
}

impl Display {
    /// A click or right-click on knob `knob` (its dial or label) selects it:
    /// the knob panel takes the keyboard, back from the browser, and with a
    /// clip open and a parameter on the knob its lane shows under the piano
    /// roll (an unassigned knob has no lane to choose) — the clip pane taking
    /// the keyboard first if the arranger had it, so the lane's own focus
    /// is a click away. The note selection is dropped then, and the
    /// marquee's duplicate span with it, so nothing in the clip view still
    /// looks like the `Delete`'s target. In a take session the lane only
    /// shows: the session's keys stay where they are, and dropping the
    /// selection there would cancel the take.
    pub(in crate::view::display) fn select_knob(&mut self, knob: usize) {
        let shows_lane = self.knob_labels_switch_lanes() && self.knob_param(knob).is_some();
        if shows_lane {
            self.set_clip_lane(ClipLane::Knob(knob));
        }
        if self.take_session.is_some() {
            return;
        }
        if self.key_focus == KeyFocus::Browser {
            self.leave_browser();
        }
        self.key_focus = KeyFocus::KnobPanel(knob);
        if !shows_lane {
            return;
        }
        if !self.view_state().is_clip_view() {
            self.focus_pane(Pane::Clip);
        }
        self.gesture.duplicate_span = None;
        if self.has_event_selection() {
            self.input_event_tx
                .send(InputEvent::ClearEventSelection)
                .ok();
        }
    }

    /// The knob panel's keys while it has the keyboard, knob `knob`
    /// selected ([`knob_panel_key`], read through
    /// [`focus_key_press`](Self::focus_key_press)). Returns whether it
    /// consumed `event`.
    pub(super) fn handle_knob_panel_key(&mut self, knob: usize, event: &InputEvent) -> bool {
        let Some((key, action)) = self.focus_key_press(event) else {
            return false;
        };
        match knob_panel_key(key, action) {
            Some(KnobPanelKey::Unassign) => {
                self.unassign_knob(knob);
                true
            }
            Some(KnobPanelKey::Leave) => {
                self.key_focus = KeyFocus::Pane;
                true
            }
            Some(KnobPanelKey::HandBack) => {
                self.key_focus = KeyFocus::Pane;
                false
            }
            None => false,
        }
    }

    /// Removes knob `knob`'s mapping on the selected track
    /// (`SetKnobParamEdit` to none, one undo step) and ends any arming. The
    /// lane goes back to velocity if it was this knob's: a lane belongs to
    /// a parameter. Nothing for a knob with no mapping. The knob stays
    /// selected.
    fn unassign_knob(&mut self, knob: usize) {
        self.disarm_knob();
        let (Some(track_idx), Some(_)) = (self.knobs_track(), self.knob_param(knob)) else {
            return;
        };
        if self.render.clip_lane == ClipLane::Knob(knob) {
            self.set_clip_lane(ClipLane::Velocity);
        }
        self.input_event_tx
            .send(InputEvent::SetKnobParam {
                track_idx,
                knob,
                param: None,
            })
            .ok();
    }

    /// The sequencer removed knob `knob`'s mapping of `param` on `track_idx`:
    /// the footer says what went (`400-parameter-knobs.md` decision 14), so
    /// a `Delete` meant as a reset shows what it did.
    pub(in crate::view::display) fn show_knob_mapping_removed(
        &mut self,
        track_idx: usize,
        knob: usize,
        param: ParamId,
        with_moves: bool,
    ) {
        let name = self.knob_param_name(track_idx, param);
        let message = removed_mapping_message(name, knob, with_moves);
        self.render.status = Some(StatusMessage::new(message));
    }
}

/// The footer's line for a removed mapping: knob `knob`'s parameter by
/// `name`, or "a parameter" when the plugin gave none, and its recorded
/// moves when any went with it. Knobs count from 1, as their placeholder
/// labels do.
fn removed_mapping_message(name: Option<&str>, knob: usize, with_moves: bool) -> String {
    let name = name.unwrap_or("a parameter");
    let moves = if with_moves { " and its moves" } else { "" };
    format!("Removed {name}{moves} from knob {}", knob + 1)
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{KnobPanelKey, knob_panel_key, removed_mapping_message};
    use crate::core::keymap::Action;

    /// Delete, whatever it is bound to and in either pane, unassigns the
    /// knob.
    #[test]
    fn delete_unassigns_the_knob() {
        for key in [Key::Delete, Key::Backspace, Key::X] {
            for action in [Action::DeleteNotes, Action::DeleteSelection] {
                assert_eq!(
                    knob_panel_key(key, Some(action)),
                    Some(KnobPanelKey::Unassign)
                );
            }
        }
    }

    /// The footer names the parameter and the knob, counted from 1.
    #[test]
    fn the_removal_names_what_went() {
        assert_eq!(
            removed_mapping_message(Some("Cutoff"), 2, false),
            "Removed Cutoff from knob 3"
        );
        assert_eq!(
            removed_mapping_message(None, 0, false),
            "Removed a parameter from knob 1"
        );
        assert_eq!(
            removed_mapping_message(Some("Cutoff"), 2, true),
            "Removed Cutoff and its moves from knob 3"
        );
    }

    /// Esc leaves at once: there is no span to drop first.
    #[test]
    fn esc_leaves() {
        assert_eq!(knob_panel_key(Key::Escape, None), Some(KnobPanelKey::Leave));
    }

    /// A pane command hands the keyboard back and acts on the pane.
    #[test]
    fn a_pane_command_hands_back() {
        for action in [
            Action::SelectAllNotes,
            Action::MuteNotes,
            Action::Quantize,
            Action::DeleteTime,
        ] {
            assert_eq!(
                knob_panel_key(Key::A, Some(action)),
                Some(KnobPanelKey::HandBack)
            );
        }
    }

    /// The arrows, `W`, the transport and undo pass through and keep the
    /// focus.
    #[test]
    fn other_keys_pass_through() {
        assert_eq!(knob_panel_key(Key::ArrowLeft, None), None);
        for action in [Action::CycleClipLane, Action::TogglePlayback, Action::Undo] {
            assert_eq!(knob_panel_key(Key::W, Some(action)), None);
        }
    }
}
