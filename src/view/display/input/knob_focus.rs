//! The knob panel's keyboard focus (`KeyFocus::KnobPanel`,
//! `390-knob-panel.md` decision 14): a click on a knob with a clip open
//! selects it — its lane shows under the piano roll, and the panel takes the
//! keyboard, as a click on a track header gives the header column it. The
//! selected knob is the one whose lane is shown, never stored. `Delete`
//! clears its CC from the lead clip; `Esc` leaves; a clip-view command
//! hands the keyboard back to the notes and acts there
//! ([`Action::leaves_knob_panel`]).

use egui::Key;

use crate::core::input_event::InputEvent;
use crate::core::keymap::Action;

use super::*;

/// What a key does while the knob panel has the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum KnobPanelKey {
    /// Clear the selected knob's CC from the lead clip.
    ClearKnob,
    /// Give the keyboard back to the notes.
    Leave,
    /// Give the keyboard back to the notes and let the key act there.
    HandBack,
}

/// The knob panel's keymap, by the action the keymap resolves the press to
/// (`action`): Delete notes (`Delete`/`⌫`, whatever it is bound to) clears
/// the knob; any other clip-view command hands the keyboard back first.
/// Esc, not a command, leaves. `None` — every other key, the transport,
/// the arrows and `W` among them — falls through to the clip view as usual.
fn knob_panel_key(key: Key, action: Option<Action>) -> Option<KnobPanelKey> {
    match (key, action) {
        (Key::Escape, _) => Some(KnobPanelKey::Leave),
        (_, Some(Action::DeleteNotes)) => Some(KnobPanelKey::ClearKnob),
        (_, Some(action)) if action.leaves_knob_panel() => Some(KnobPanelKey::HandBack),
        _ => None,
    }
}

impl Display {
    /// A click on knob `knob` (its dial or label) with a clip open selects
    /// it: its lane shows and the knob panel takes the keyboard — the clip
    /// pane's first, if the arranger had it, and back from the browser. The
    /// note selection is dropped, and the marquee's duplicate span with it,
    /// so the `Delete` that follows can't be read as one on the notes and
    /// nothing in the clip view still looks selected. In a take session the lane only shows: the
    /// session's keys stay where they are, and dropping the selection there
    /// would cancel the take.
    pub(in crate::view::display) fn select_knob(&mut self, knob: usize) {
        self.set_clip_lane(ClipLane::Knob(knob));
        if self.take_session.is_some() {
            return;
        }
        if self.key_focus == KeyFocus::Browser {
            self.leave_browser();
        }
        if !self.view_state().is_clip_view() {
            self.focus_pane(Pane::Clip);
        }
        self.key_focus = KeyFocus::KnobPanel;
        self.gesture.duplicate_span = None;
        if self.has_event_selection() {
            self.input_event_tx
                .send(InputEvent::ClearEventSelection)
                .ok();
        }
    }

    /// The knob panel's keys while it has the keyboard ([`knob_panel_key`]).
    /// Returns whether it consumed `event`. A clipboard chord hands the
    /// keyboard back and goes on to the notes, as a note command does.
    pub(super) fn handle_knob_panel_key(&mut self, event: &InputEvent) -> bool {
        let (key, modifiers) = match *event {
            InputEvent::KeyPressed { key, modifiers } => (key, modifiers),
            InputEvent::Copy { .. } | InputEvent::Cut { .. } | InputEvent::Paste => {
                self.key_focus = KeyFocus::Pane;
                return false;
            }
            _ => return false,
        };
        // Esc during an audio export goes on to cancel it.
        if key == Key::Escape && self.is_exporting_audio() {
            return false;
        }
        match knob_panel_key(key, self.pressed_action(key, modifiers)) {
            Some(KnobPanelKey::ClearKnob) => {
                // The whole clip, material outside the window included: the
                // lane's `⌘A` then `Delete`, one undo step. Nothing for a clip
                // with no events.
                let (start, end) = self.render.lead_clip_extent;
                if let Some(controller) = self.clip_lane_controller()
                    && start < end
                {
                    self.input_event_tx
                        .send(InputEvent::ClearControllerSpan {
                            controller,
                            start,
                            end,
                        })
                        .ok();
                }
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
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{KnobPanelKey, knob_panel_key};
    use crate::core::keymap::Action;

    /// Delete notes, whatever it is bound to, clears the knob.
    #[test]
    fn delete_clears_the_knob() {
        for key in [Key::Delete, Key::Backspace, Key::X] {
            assert_eq!(
                knob_panel_key(key, Some(Action::DeleteNotes)),
                Some(KnobPanelKey::ClearKnob)
            );
        }
    }

    /// Esc leaves at once: there is no span to drop first.
    #[test]
    fn esc_leaves() {
        assert_eq!(knob_panel_key(Key::Escape, None), Some(KnobPanelKey::Leave));
    }

    /// A note command hands the keyboard back and acts on the notes.
    #[test]
    fn a_note_command_hands_back() {
        for action in [Action::SelectAllNotes, Action::MuteNotes, Action::Quantize] {
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
