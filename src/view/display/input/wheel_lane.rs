//! The wheel lane's keyboard focus (`KeyFocus::WheelLane`) and its span: a
//! press in the PB or MW lane gives it the keyboard, a press-drag there
//! marks a span of the shown wheel, `⌘A` marks the whole curve, `Delete`
//! returns the span to rest (`InputEvent::RestWheel`), `Esc` drops the span
//! and then the focus. A note command takes the keyboard back to the notes
//! and acts there ([`Action::acts_on_notes`]). See `380-wheel-lane.md`
//! decisions 14–19.

use egui::Key;

use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::keymap::Action;

use super::*;

/// What a key does while the wheel lane has the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum WheelLaneKey {
    /// Return the span to rest (nothing without one).
    RestSpan,
    /// Mark the clip's whole extent.
    MarkAll,
    /// Drop the span.
    DropSpan,
    /// Give the keyboard back to the notes.
    Leave,
    /// Give the keyboard back to the notes and let the key act there.
    HandBack,
}

/// The wheel lane's keymap: plain Delete/Backspace returns the span to rest,
/// `⌘A` (`SelectAllNotes`, whatever it is bound to) marks the whole curve,
/// Esc drops the span or, with none, leaves; a note command, an arrow with
/// notes selected, or a modified Delete hands the keyboard back first.
/// `None` — every other key, the transport and `W` among them — falls
/// through to the clip view as usual. `action` is what the keymap resolves
/// the press to.
fn wheel_lane_key(
    key: Key,
    modifiers: KeyModifiers,
    action: Option<Action>,
    has_span: bool,
    has_event_selection: bool,
) -> Option<WheelLaneKey> {
    let plain = !modifiers.command && !modifiers.alt && !modifiers.shift;
    let arrow = matches!(
        key,
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown
    );
    match key {
        Key::Delete | Key::Backspace if plain => Some(WheelLaneKey::RestSpan),
        Key::Escape if has_span => Some(WheelLaneKey::DropSpan),
        Key::Escape => Some(WheelLaneKey::Leave),
        _ if action == Some(Action::SelectAllNotes) => Some(WheelLaneKey::MarkAll),
        _ if action.is_some_and(Action::acts_on_notes) => Some(WheelLaneKey::HandBack),
        Key::Delete | Key::Backspace => Some(WheelLaneKey::HandBack),
        _ if arrow && has_event_selection => Some(WheelLaneKey::HandBack),
        _ => None,
    }
}

/// The span a drag from `anchor` to `tick` (both snapped) marks, either way
/// round; `None` while they meet, so a span is never empty.
fn span_between(anchor: i32, tick: i32) -> Option<(i32, i32)> {
    (tick != anchor).then(|| (anchor.min(tick), anchor.max(tick)))
}

impl Display {
    /// Whether the wheel lane has the keyboard.
    pub(in crate::view::display) fn wheel_lane_has_keyboard(&self) -> bool {
        self.key_owner() == KeyFocus::WheelLane
    }

    /// Shows `lane` under the piano roll — a gutter label or `W`. Velocity
    /// gives the keyboard back to the notes; a switch between the wheels
    /// keeps the focus but drops the span, which was the other wheel's.
    pub(in crate::view::display) fn set_clip_lane(&mut self, lane: ClipLane) {
        if lane == self.render.clip_lane {
            return;
        }
        self.render.clip_lane = lane;
        self.clear_wheel_span();
        if lane.wheel().is_none() && self.key_focus == KeyFocus::WheelLane {
            self.key_focus = KeyFocus::Pane;
        }
    }

    /// A press in a wheel lane at `x`: the lane takes the keyboard and the
    /// span is dropped, anchored at the snapped tick for a drag to mark one
    /// ([`extend_wheel_span`](Self::extend_wheel_span)). Clip pane only.
    pub(super) fn begin_wheel_span(&mut self, x: f32) {
        self.key_focus = KeyFocus::WheelLane;
        self.gesture.wheel_span = None;
        self.gesture.wheel_span_anchor = Some(self.snapped_tick_at(x));
    }

    /// A drag from a wheel-lane press: the span from the anchor to the
    /// snapped tick under `x`, either way round; none while they meet. A
    /// no-op with no press. Clip pane only.
    pub(super) fn extend_wheel_span(&mut self, x: f32) {
        let Some(anchor) = self.gesture.wheel_span_anchor else {
            return;
        };
        self.gesture.wheel_span = span_between(anchor, self.snapped_tick_at(x));
    }

    /// Drops the wheel span and any drag marking one.
    pub(in crate::view::display) fn clear_wheel_span(&mut self) {
        self.gesture.wheel_span = None;
        self.gesture.wheel_span_anchor = None;
    }

    /// The span goes with the focus: once anything else has the keyboard, a
    /// span left over is dropped. Run each frame before the input.
    pub(super) fn drop_stale_wheel_span(&mut self) {
        if self.key_focus != KeyFocus::WheelLane {
            self.clear_wheel_span();
        }
    }

    /// The wheel lane's keys while it has the keyboard ([`wheel_lane_key`]).
    /// Returns whether it consumed `event`. A clipboard chord hands the
    /// keyboard back and goes on to the notes, as a note command does.
    pub(super) fn handle_wheel_lane_key(&mut self, event: &InputEvent) -> bool {
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
        let action = self.pressed_action(key, modifiers);
        let has_span = self.gesture.wheel_span.is_some();
        match wheel_lane_key(key, modifiers, action, has_span, self.has_event_selection()) {
            Some(WheelLaneKey::RestSpan) => {
                if let (Some((start, end)), Some(wheel)) =
                    (self.gesture.wheel_span, self.render.clip_lane.wheel())
                {
                    self.input_event_tx
                        .send(InputEvent::RestWheel { wheel, start, end })
                        .ok();
                    self.gesture.wheel_span = None;
                }
                true
            }
            Some(WheelLaneKey::MarkAll) => {
                let (start, end) = self.render.lead_clip_extent;
                self.gesture.wheel_span = (start < end).then_some((start, end));
                true
            }
            Some(WheelLaneKey::DropSpan) => {
                self.clear_wheel_span();
                true
            }
            Some(WheelLaneKey::Leave) => {
                self.key_focus = KeyFocus::Pane;
                true
            }
            Some(WheelLaneKey::HandBack) => {
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

    use super::{WheelLaneKey, span_between, wheel_lane_key};
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::Action;

    const PLAIN: KeyModifiers = KeyModifiers {
        shift: false,
        command: false,
        alt: false,
        ctrl: false,
    };

    fn key(
        key: Key,
        action: Option<Action>,
        has_span: bool,
        selected: bool,
    ) -> Option<WheelLaneKey> {
        wheel_lane_key(key, PLAIN, action, has_span, selected)
    }

    /// Delete and Backspace rest the span, whether there is one or not (the
    /// press is the lane's either way: it never deletes notes).
    #[test]
    fn delete_rests_the_span() {
        for k in [Key::Delete, Key::Backspace] {
            assert_eq!(
                key(k, Some(Action::DeleteNotes), true, true),
                Some(WheelLaneKey::RestSpan)
            );
            assert_eq!(key(k, None, false, false), Some(WheelLaneKey::RestSpan));
        }
    }

    /// Esc drops the span first, then the focus.
    #[test]
    fn escape_drops_the_span_then_leaves() {
        assert_eq!(
            key(Key::Escape, None, true, false),
            Some(WheelLaneKey::DropSpan)
        );
        assert_eq!(
            key(Key::Escape, None, false, false),
            Some(WheelLaneKey::Leave)
        );
    }

    /// `⌘A` marks the whole curve: the action, not a hard-coded chord.
    #[test]
    fn select_all_marks_the_curve() {
        assert_eq!(
            key(Key::A, Some(Action::SelectAllNotes), false, false),
            Some(WheelLaneKey::MarkAll)
        );
    }

    /// A note command hands the keyboard back; the transport, `W` and undo
    /// stay with the lane.
    #[test]
    fn note_commands_hand_the_keyboard_back() {
        for action in [Action::MuteNotes, Action::Quantize, Action::DuplicateNotes] {
            assert_eq!(
                key(Key::M, Some(action), true, true),
                Some(WheelLaneKey::HandBack)
            );
        }
        for action in [Action::TogglePlayback, Action::CycleClipLane, Action::Undo] {
            assert_eq!(key(Key::W, Some(action), true, true), None);
        }
    }

    /// The arrows edit selected notes, so they hand back then; with none
    /// selected they move the cursor from the lane.
    #[test]
    fn arrows_hand_back_only_with_notes_selected() {
        assert_eq!(
            key(Key::ArrowUp, None, false, true),
            Some(WheelLaneKey::HandBack)
        );
        assert_eq!(key(Key::ArrowLeft, None, false, false), None);
    }

    /// A modified Delete is a command (`⌘Delete`), never the lane's.
    #[test]
    fn modified_delete_hands_back() {
        let cmd = KeyModifiers {
            command: true,
            ..PLAIN
        };
        assert_eq!(
            wheel_lane_key(Key::Delete, cmd, None, true, false),
            Some(WheelLaneKey::HandBack)
        );
    }

    /// A drag marks the span between its press and the pointer, leftwards
    /// too, and none while it is back on the press.
    #[test]
    fn a_drag_marks_the_span_either_way() {
        assert_eq!(span_between(480, 960), Some((480, 960)));
        assert_eq!(span_between(960, 480), Some((480, 960)));
        assert_eq!(span_between(480, 480), None);
    }
}
