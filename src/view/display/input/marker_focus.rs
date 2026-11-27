//! The arranger's marker strip under the pointer and the keyboard
//! (`KeyFocus::Markers`, `600-markers.md` decisions 6–8). A press on a flag
//! selects its marker and gives the strip the keyboard; released where it
//! was, it recalls the marker as its digit does, and dragged past the latch
//! it moves it, previewed until the release commits it as one undo step. A
//! press on the strip's `MARKERS` label gives it the keyboard too. With it:
//! `Delete` removes the selected marker and selects the one before it (else
//! after), `⌘R` names it, `←`/`→` select its neighbours; `Esc` (or Tab, a
//! pane command, a click elsewhere) gives the keyboard back to the pane.

use std::borrow::Cow;

use egui::Key;

use crate::core::input_event::{InputEvent, is_pane_focus_key};
use crate::core::keymap::Action;
use crate::models::markers::MarkerChange;

use super::*;

/// What a key does while the marker strip has the keyboard.
#[derive(Debug, PartialEq, Eq)]
enum MarkerStripKey {
    /// Put back the marker being dragged; nothing is committed.
    CancelDrag,
    /// Remove the selected marker.
    Remove,
    /// Open the rename field over the selected marker.
    Rename,
    /// Select the marker before (`true`) or after the selected one.
    Select {
        /// Toward the start.
        before: bool,
    },
    /// Give the keyboard back to the pane.
    Leave {
        /// False when the key still goes on to do its usual work.
        consumed: bool,
    },
}

/// The marker strip's keymap, by the key and the action the keymap
/// resolves it to (`action`), with a flag drag held or not (`dragging`):
/// `Esc` cancels the drag, else leaves; Delete (`Delete`/`⌫`, whatever it is
/// bound to) removes, `⌘R` (Rename track) names; plain `←`/`→` select; Tab
/// and every pane command ([`Action::leaves_marker_strip`]) hand the
/// keyboard back and act. `None` — the digits, `⌘1`–`⌘9`, the transport, the
/// zoom — falls through to the pane as usual.
fn marker_strip_key(
    key: Key,
    modifiers: KeyModifiers,
    action: Option<Action>,
    dragging: bool,
) -> Option<MarkerStripKey> {
    let plain = !modifiers.command && !modifiers.alt && !modifiers.shift;
    match (key, action) {
        (Key::Escape, _) if dragging => Some(MarkerStripKey::CancelDrag),
        (Key::Escape, _) => Some(MarkerStripKey::Leave { consumed: true }),
        (Key::Tab, _) if modifiers.shift || is_pane_focus_key(key, modifiers) => {
            Some(MarkerStripKey::Leave { consumed: false })
        }
        (Key::ArrowLeft | Key::ArrowRight, _) if plain => Some(MarkerStripKey::Select {
            before: key == Key::ArrowLeft,
        }),
        (_, Some(Action::DeleteSelection | Action::DeleteNotes)) => Some(MarkerStripKey::Remove),
        (_, Some(Action::RenameTrack)) => Some(MarkerStripKey::Rename),
        (_, Some(action)) if action.leaves_marker_strip() => {
            Some(MarkerStripKey::Leave { consumed: false })
        }
        _ => None,
    }
}

impl Display {
    /// Selects marker `number` and gives the marker strip the keyboard,
    /// back from the browser if it had it.
    fn select_marker(&mut self, number: u8) {
        if self.key_focus == KeyFocus::Browser {
            self.leave_browser();
        }
        self.key_focus = KeyFocus::Markers(number);
    }

    /// A press on the marker strip at `(x, y)`, from `handle_mouse_click`:
    /// on a flag, selects its marker and holds it for a drag or a click
    /// (`begin_marker_drag`); on the `MARKERS` label beside the strip,
    /// selects the marker at or before the cursor (else the first) without
    /// moving anything. Anywhere else in the strip, nothing, and the
    /// keyboard goes back to the pane (`600-markers.md` decisions 6 and 7).
    /// Hit-tested before the focus changes, with the selected flag still on
    /// top, so a press where it covers another takes it, the one that shows.
    pub(super) fn handle_marker_strip_press(&mut self, x: f32, y: f32) {
        if let Some(number) = self.marker_flag_at(x, y) {
            self.select_marker(number);
            self.begin_marker_drag(number, x, y);
        } else if x < self.content_origin_x()
            && let Some(number) = self.markers.at_or_before(self.render_cursor_tick())
        {
            self.select_marker(number);
        } else {
            self.key_focus = KeyFocus::Pane;
        }
    }

    /// Holds marker `number` for a drag from `(x, y)`. Nothing moves or is
    /// recalled until the pointer moves past the latch or is released; the
    /// cursor line hides, as for the loop band's drag.
    fn begin_marker_drag(&mut self, number: u8, x: f32, y: f32) {
        let Some(from) = self.markers.get(number).map(|marker| marker.tick) else {
            return;
        };
        self.gesture.hover_cursor = None;
        self.gesture.marker_drag = Some(MarkerDrag::new(
            number,
            (x, y),
            from,
            self.screen_x_to_tick(x),
        ));
    }

    /// Follows a held flag to the pointer at `(x, y)`: past the latch, the
    /// marker's preview moves on the zoom-adaptive arranger grid
    /// (`cursor_grid_ticks`).
    pub(super) fn extend_marker_drag(&mut self, x: f32, y: f32) {
        let Some(origin) = self.gesture.marker_drag.map(|drag| drag.origin) else {
            return;
        };
        let pointer_tick = self.screen_x_to_tick(x);
        let grid = self.cursor_grid_ticks();
        let past_threshold = past_drag_threshold(origin, (x, y), Self::DRAG_THRESHOLD_PX);
        if let Some(drag) = self.gesture.marker_drag.as_mut() {
            drag.extend(pointer_tick, grid, past_threshold);
        }
    }

    /// Ends a held flag on release: a drag commits the move
    /// (`MarkerChange::Move`, one undo step; nothing when it came back to
    /// where it was), a click recalls the marker as its digit does.
    pub(super) fn finish_marker_drag(&mut self) {
        let Some(drag) = self.gesture.marker_drag.take() else {
            return;
        };
        // Removed while held (`Delete` takes the press with it, but a
        // marker can go by other ways too): nothing to move or recall.
        if self.markers.get(drag.number).is_none() {
            return;
        }
        match drag.drop_tick() {
            Some(tick) => self.change_marker(drag.number, MarkerChange::Move(tick)),
            None if drag.dragging => {}
            None => {
                self.input_event_tx
                    .send(InputEvent::Action(Action::RecallMarker(drag.number)))
                    .ok();
            }
        }
    }

    /// Sends `change` to marker `number` (`InputEvent::ChangeMarker`, one
    /// undo step) and makes it in the view's mirror at once. The mirror is
    /// otherwise a frame or two behind (`UiEvent::MarkersChanged`), and a
    /// dropped flag flickered back to where it was before landing. The
    /// sequencer's list replaces this one when it arrives; it applies the
    /// same `Markers::change`, so the two agree.
    pub(in crate::view::display) fn change_marker(&mut self, number: u8, change: MarkerChange) {
        self.markers.change(number, &change);
        // The sequencer answers each with exactly one list, in order
        // (`follow_markers`).
        self.markers_in_flight += 1;
        self.input_event_tx
            .send(InputEvent::ChangeMarker { number, change })
            .ok();
    }

    /// Takes the sequencer's markers (`UiEvent::MarkersChanged`) and moves
    /// the strip's selection with them while it has the keyboard
    /// ([`follow_selection`]): an undo of a `Delete` selects the marker it
    /// brings back, so it is on top again where it overlaps, and a redo
    /// moves the selection on as the `Delete` did. The answers to the
    /// strip's own changes, which the mirror already holds, move nothing
    /// ([`answer_kind`]): one older than the mirror would read as a marker
    /// coming back.
    pub(in crate::view::display) fn follow_markers(&mut self, markers: Markers) {
        match answer_kind(&mut self.markers_in_flight) {
            MarkersAnswer::Stale => return,
            MarkersAnswer::Own => {}
            MarkersAnswer::Other => match self.key_focus {
                KeyFocus::Markers(selected) => {
                    let next =
                        follow_selection(selected, &self.markers, &markers, &self.markers_deleted);
                    self.key_focus = next.map_or(KeyFocus::Pane, KeyFocus::Markers);
                }
                _ => self.markers_deleted.clear(),
            },
        }
        self.markers = markers;
    }

    /// The markers as the strip shows them: with a flag dragged, the
    /// marker at its drop, any it would replace gone — the drop's own rule,
    /// so the preview is what the release commits.
    pub(in crate::view::display) fn shown_markers(&self) -> Cow<'_, Markers> {
        match self.gesture.marker_drag {
            Some(drag) if drag.dragging => {
                let mut markers = self.markers.clone();
                markers.change(drag.number, &MarkerChange::Move(drag.target));
                Cow::Owned(markers)
            }
            _ => Cow::Borrowed(&self.markers),
        }
    }

    /// The marker strip's keys while it has the keyboard, marker `number`
    /// selected ([`marker_strip_key`]). Returns whether it consumed `event`.
    pub(super) fn handle_marker_strip_key(&mut self, number: u8, event: &InputEvent) -> bool {
        let Some((key, modifiers)) = (match *event {
            InputEvent::KeyPressed { key, modifiers } => Some((key, modifiers)),
            _ => None,
        }) else {
            return false;
        };
        let action = self.pressed_action(key, modifiers);
        let dragging = self.gesture.marker_drag.is_some();
        match marker_strip_key(key, modifiers, action, dragging) {
            Some(MarkerStripKey::CancelDrag) => {
                self.gesture.marker_drag = None;
                true
            }
            Some(MarkerStripKey::Remove) => {
                if self
                    .gesture
                    .marker_drag
                    .is_some_and(|drag| drag.number == number)
                {
                    self.gesture.marker_drag = None;
                }
                self.markers_deleted.push(number);
                self.key_focus = match self.markers.selected_after_removing(number) {
                    Some(next) => KeyFocus::Markers(next),
                    None => KeyFocus::Pane,
                };
                self.change_marker(number, MarkerChange::Remove);
                true
            }
            Some(MarkerStripKey::Rename) => {
                self.open_marker_rename(number);
                true
            }
            Some(MarkerStripKey::Select { before }) => {
                if let Some(next) = self.markers.neighbour(number, before) {
                    self.key_focus = KeyFocus::Markers(next);
                }
                true
            }
            Some(MarkerStripKey::Leave { consumed }) => {
                self.key_focus = KeyFocus::Pane;
                consumed
            }
            None => false,
        }
    }
}

/// What a list from the sequencer is to the view's mirror
/// ([`answer_kind`]).
#[derive(Debug, PartialEq, Eq)]
enum MarkersAnswer {
    /// The answer to one of the strip's changes, with more of them still to
    /// answer: older than the mirror, which already holds those. Dropped.
    Stale,
    /// The answer to the strip's last change: what the mirror holds.
    Own,
    /// Anything else: an undo, a redo, a set, a time edit, a load.
    Other,
}

/// Which answer the next list is, given the strip's changes not yet
/// answered (`in_flight`), counting it off. The sequencer answers each
/// change with exactly one list, in order. A list it sent on its own just
/// before a change can be taken for that change's answer; the next one then
/// reads as `Other`, which is only a selection follow on a list the mirror
/// already holds.
fn answer_kind(in_flight: &mut usize) -> MarkersAnswer {
    match *in_flight {
        0 => MarkersAnswer::Other,
        1 => {
            *in_flight = 0;
            MarkersAnswer::Own
        }
        _ => {
            *in_flight -= 1;
            MarkersAnswer::Stale
        }
    }
}

/// The selected marker after the markers go from `old` to `new`: a marker
/// the strip deleted (`deleted`) back, if exactly one marker came back (an
/// undo of a `Delete`; a new one from `⌘n` is not selected); else, when the
/// selected marker is gone (a redo of a `Delete`, an undo of a set), the
/// one before it, else after, as `Delete` picks; else the same one. `None`
/// when nothing is left to select.
fn follow_selection(selected: u8, old: &Markers, new: &Markers, deleted: &[u8]) -> Option<u8> {
    let mut back = new
        .as_slice()
        .iter()
        .map(|marker| marker.number)
        .filter(|&number| old.get(number).is_none());
    if let (Some(number), None) = (back.next(), back.next())
        && deleted.contains(&number)
    {
        return Some(number);
    }
    if new.get(selected).is_some() {
        return Some(selected);
    }
    old.selected_after_removing(selected)
        .filter(|&number| new.get(number).is_some())
}

#[cfg(test)]
mod tests {
    use egui::Key;

    use super::{MarkerStripKey, MarkersAnswer, answer_kind, follow_selection, marker_strip_key};
    use crate::core::input_event::KeyModifiers;
    use crate::core::keymap::Action;
    use crate::models::markers::Markers;

    #[test]
    fn esc_cancels_a_drag_before_it_leaves() {
        assert_eq!(
            marker_strip_key(Key::Escape, KeyModifiers::default(), None, true),
            Some(MarkerStripKey::CancelDrag)
        );
        assert_eq!(
            marker_strip_key(Key::Escape, KeyModifiers::default(), None, false),
            Some(MarkerStripKey::Leave { consumed: true })
        );
    }

    /// Delete, whatever it is bound to, removes the marker; `⌘R` names it.
    #[test]
    fn delete_removes_and_rename_names() {
        for action in [Action::DeleteSelection, Action::DeleteNotes] {
            assert_eq!(
                marker_strip_key(Key::Backspace, KeyModifiers::default(), Some(action), false),
                Some(MarkerStripKey::Remove)
            );
        }
        let command = KeyModifiers {
            command: true,
            ..KeyModifiers::default()
        };
        assert_eq!(
            marker_strip_key(Key::R, command, Some(Action::RenameTrack), false),
            Some(MarkerStripKey::Rename)
        );
    }

    /// Plain arrows select; with a modifier they are the pane's.
    #[test]
    fn plain_arrows_select_the_neighbours() {
        assert_eq!(
            marker_strip_key(Key::ArrowLeft, KeyModifiers::default(), None, false),
            Some(MarkerStripKey::Select { before: true })
        );
        assert_eq!(
            marker_strip_key(Key::ArrowRight, KeyModifiers::default(), None, false),
            Some(MarkerStripKey::Select { before: false })
        );
        let alt = KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        };
        assert_eq!(marker_strip_key(Key::ArrowLeft, alt, None, false), None);
    }

    /// Tab and a pane command hand the keyboard back and still act; the
    /// marker keys and the transport stay with the strip.
    #[test]
    fn tab_and_pane_commands_hand_back() {
        assert_eq!(
            marker_strip_key(Key::Tab, KeyModifiers::default(), None, false),
            Some(MarkerStripKey::Leave { consumed: false })
        );
        assert_eq!(
            marker_strip_key(
                Key::A,
                KeyModifiers::default(),
                Some(Action::SelectAllClips),
                false
            ),
            Some(MarkerStripKey::Leave { consumed: false })
        );
        for action in [
            Action::RecallMarker(2),
            Action::SetMarker(2),
            Action::TogglePlayback,
        ] {
            assert_eq!(
                marker_strip_key(Key::Num2, KeyModifiers::default(), Some(action), false),
                None
            );
        }
    }

    /// Delete 2 (selecting 1), undo, redo, undo: the selection follows the
    /// marker that comes and goes, so it is the one on top each time.
    #[test]
    fn undo_and_redo_of_a_delete_carry_the_selection() {
        let both = Markers::from_placed(&[(1, 0), (2, 100)]);
        let one = Markers::from_placed(&[(1, 0)]);
        // The deleted set keeps 2 through the undo, so the redo and the
        // undo after it go on following it.
        assert_eq!(follow_selection(1, &one, &both, &[2]), Some(2));
        assert_eq!(follow_selection(2, &both, &one, &[2]), Some(1));
        assert_eq!(follow_selection(1, &one, &both, &[2]), Some(2));
    }

    /// Two deletes undone come back in turn, each selected as it returns.
    #[test]
    fn chained_deletes_unwind_in_order() {
        let all = Markers::from_placed(&[(1, 0), (2, 100), (3, 200)]);
        let two = Markers::from_placed(&[(1, 0), (2, 100)]);
        let one = Markers::from_placed(&[(1, 0)]);
        assert_eq!(follow_selection(1, &one, &two, &[3, 2]), Some(2));
        assert_eq!(follow_selection(2, &two, &all, &[3]), Some(3));
    }

    /// A change that leaves the selected marker and brings nothing back
    /// keeps the selection (the echo of the strip's own change too); one
    /// that leaves nothing selects nothing.
    #[test]
    fn otherwise_the_selection_stays_or_goes() {
        let both = Markers::from_placed(&[(1, 0), (2, 100)]);
        let moved = Markers::from_placed(&[(1, 0), (2, 400)]);
        assert_eq!(follow_selection(2, &both, &moved, &[]), Some(2));
        assert_eq!(follow_selection(2, &both, &both, &[]), Some(2));
        assert_eq!(follow_selection(2, &both, &Markers::default(), &[]), None);
    }

    /// A marker `⌘n` makes is new, not one the strip deleted: the
    /// selection stays where it was.
    #[test]
    fn a_new_marker_leaves_the_selection() {
        let one = Markers::from_placed(&[(2, 0)]);
        let set = Markers::from_placed(&[(2, 0), (5, 960)]);
        assert_eq!(follow_selection(2, &one, &set, &[]), Some(2));
        assert_eq!(follow_selection(2, &one, &set, &[7]), Some(2));
    }

    /// Two quick changes: the first answer is older than the mirror and is
    /// dropped, the second is the mirror's own; after that, lists are the
    /// sequencer's own news.
    #[test]
    fn answers_to_quick_changes_count_off() {
        let mut in_flight = 2;
        assert_eq!(answer_kind(&mut in_flight), MarkersAnswer::Stale);
        assert_eq!(answer_kind(&mut in_flight), MarkersAnswer::Own);
        assert_eq!(answer_kind(&mut in_flight), MarkersAnswer::Other);
        assert_eq!(in_flight, 0);
    }
}
