//! The piano roll's note edits: insert notes (a double-click, a paste), copy
//! the selection for the note clipboard, and move or resize an explicit set
//! of notes by one drag.
//!
//! Unlike `edits.rs`, these take explicit ids rather than reading
//! `self.event_selection` (a drag can target a note the selection doesn't
//! hold yet), and they keep same-pitch notes from overlapping: wherever an
//! edited note and another note of the same pitch overlap, the earlier one is
//! trimmed to end where the later one starts, and with equal starts the
//! edited note wins and the other is removed. Overlaps between two untouched
//! notes are left alone. Every pairing is worked out *before* the edit moves
//! anything, because a transient same-pitch overlap would make
//! [`pair_note_events`](Clip::pair_note_events)'s per-pitch stack match the
//! wrong `NoteOff`. The undoable wrappers live in
//! `core/sequencer/edit/event_edits.rs`; see `240-release-plan.md` § A1.

use std::collections::HashSet;

use uuid::Uuid;

use crate::models::event::{Event, EventType};

use super::{Clip, pairing::NotePairing};

/// One note as `(start, end, pitch)` — event ticks and note number — the
/// shape [`NoteDrag::dragged_spans`] works on.
pub(crate) type NoteBounds = (i32, i32, u8);

/// The change one note drag makes to every note it targets. Deltas are in
/// ticks / semitones, relative to where the notes were when the drag began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteDrag {
    /// Shift each note in time and pitch, keeping its length. The whole group
    /// moves by one clamped delta, so the notes keep their relative shape.
    Move {
        /// Time shift, in ticks.
        delta_ticks: i32,
        /// Pitch shift, in semitones.
        delta_pitch: i32,
    },
    /// Move each note's start by `delta_ticks`, keeping its end.
    ResizeStart {
        /// Start shift, in ticks.
        delta_ticks: i32,
    },
    /// Move each note's end by `delta_ticks`, keeping its start.
    ResizeEnd {
        /// End shift, in ticks.
        delta_ticks: i32,
    },
}

/// One note as indices into `Clip::events`, with its pitch and span, for the
/// overlap pass.
#[derive(Clone, Copy)]
struct NoteSpan {
    /// Index of the `NoteOn`.
    on_idx: usize,
    /// Index of the paired `NoteOff`, if it has one.
    off_idx: Option<usize>,
    /// Note number.
    pitch: u8,
    /// Event tick of the `NoteOn`.
    start: i32,
    /// Event tick the note ends at.
    end: i32,
}

/// One note on the note clipboard ([`Clip::copy_selected_notes`]), placed
/// relative to the copy's earliest note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopiedNote {
    /// Ticks after the copy's earliest note start.
    pub(crate) offset: i32,
    /// Length in ticks.
    pub(crate) length: i32,
    /// The `NoteOn`'s MIDI bytes (status, pitch, velocity).
    pub(crate) midi_message: Vec<u8>,
    /// Whether the note was muted.
    pub(crate) muted: bool,
}

impl NoteDrag {
    /// Where this drag puts each of `notes` (`(start, end, pitch)`, in order)
    /// inside the clip window `(window_start, window_end)` — the one clamping
    /// rule both [`Clip::apply_note_drag`] and the piano roll's drag preview
    /// use, so the preview lands exactly where the release commits.
    ///
    /// A move clamps one delta for the whole group, so every start stays
    /// inside the window and every pitch in 0–127 — a note already outside
    /// the window can move back in but is never pushed further out. A resize
    /// clamps per note: a start stays inside the window and before its end,
    /// an end stays after its start, and a growing end stops at the window
    /// end (one already past it may shrink but not grow). Same-pitch overlaps
    /// are not resolved here.
    pub(crate) fn dragged_spans(
        self,
        notes: &[NoteBounds],
        (window_start, window_end): (i32, i32),
    ) -> Vec<NoteBounds> {
        match self {
            NoteDrag::Move {
                delta_ticks,
                delta_pitch,
            } => {
                let (min_start, max_start) =
                    min_max(notes.iter().map(|&(start, ..)| start)).unwrap_or((0, 0));
                let delta_ticks = delta_ticks.clamp(
                    (window_start - min_start).min(0),
                    (window_end - 1 - max_start).max(0),
                );
                let (min_pitch, max_pitch) =
                    min_max(notes.iter().map(|&(.., pitch)| i32::from(pitch))).unwrap_or((0, 0));
                let delta_pitch = delta_pitch.clamp(-min_pitch, 127 - max_pitch);
                notes
                    .iter()
                    .map(|&(start, end, pitch)| {
                        let pitch = (i32::from(pitch) + delta_pitch) as u8;
                        (start + delta_ticks, end + delta_ticks, pitch)
                    })
                    .collect()
            }
            NoteDrag::ResizeStart { delta_ticks } => notes
                .iter()
                .map(|&(start, end, pitch)| {
                    let lowest = window_start.min(start);
                    let start = (start + delta_ticks).clamp(lowest, (end - 1).max(lowest));
                    (start, end, pitch)
                })
                .collect(),
            NoteDrag::ResizeEnd { delta_ticks } => notes
                .iter()
                .map(|&(start, end, pitch)| {
                    let highest = window_end.max(end).max(start + 1);
                    (start, (end + delta_ticks).clamp(start + 1, highest), pitch)
                })
                .collect(),
        }
    }

    /// The same kind of drag with new deltas; a resize ignores `delta_pitch`.
    pub(crate) fn with_deltas(self, delta_ticks: i32, delta_pitch: i32) -> NoteDrag {
        match self {
            NoteDrag::Move { .. } => NoteDrag::Move {
                delta_ticks,
                delta_pitch,
            },
            NoteDrag::ResizeStart { .. } => NoteDrag::ResizeStart { delta_ticks },
            NoteDrag::ResizeEnd { .. } => NoteDrag::ResizeEnd { delta_ticks },
        }
    }

    /// Whether the drag changes nothing (every delta zero).
    pub(crate) fn is_noop(self) -> bool {
        match self {
            NoteDrag::Move {
                delta_ticks,
                delta_pitch,
            } => delta_ticks == 0 && delta_pitch == 0,
            NoteDrag::ResizeStart { delta_ticks } | NoteDrag::ResizeEnd { delta_ticks } => {
                delta_ticks == 0
            }
        }
    }
}

impl Clip {
    /// A new note pair for [`insert_notes`](Self::insert_notes): `NoteOn` at
    /// `tick` and `NoteOff` `length` ticks later, at `pitch` and `velocity`,
    /// on channel 1 (playback re-channels to the track). The start is clamped
    /// into the window, the end to the window end, the length to at least one
    /// tick, the pitch to 0–127 and the velocity to 1–127. Returns
    /// `(note_on, note_off)`.
    pub(crate) fn new_note(
        &self,
        tick: i32,
        length: i32,
        pitch: i32,
        velocity: i32,
    ) -> (Event, Event) {
        let (window_start, window_end) = (self.region.start(), self.region.end());
        let start = tick.clamp(window_start, (window_end - 1).max(window_start));
        let pitch = pitch.clamp(0, 127) as u8;
        let velocity = velocity.clamp(1, 127) as u8;
        self.note_pair(start, length, vec![0x90, pitch, velocity])
    }

    /// A `NoteOn` with `midi_message` at `start` and its `NoteOff` `length`
    /// ticks later — the end stopped at the window end, the length at least
    /// one tick. The one rule [`new_note`](Self::new_note) and
    /// [`pasted_notes`](Self::pasted_notes) build their notes by.
    fn note_pair(&self, start: i32, length: i32, midi_message: Vec<u8>) -> (Event, Event) {
        let end = (start + length).min(self.region.end()).max(start + 1);
        let note_on = Event::new(start, end - start, midi_message);
        let note_off = Self::note_off_for(&note_on, end);
        (note_on, note_off)
    }

    /// Adds the notes `(note_on, note_off)` (built by
    /// [`new_note`](Self::new_note) or [`pasted_notes`](Self::pasted_notes)),
    /// resolving same-pitch overlaps with them.
    pub(crate) fn insert_notes(&mut self, notes: impl IntoIterator<Item = (Event, Event)>) {
        let pairing = self.pair_note_events();
        let mut spans = self.note_spans(&pairing);

        let mut targets = Vec::new();
        for (note_on, note_off) in notes {
            let on_idx = self.events.len();
            spans.push(NoteSpan {
                on_idx,
                off_idx: Some(on_idx + 1),
                pitch: note_on.note_number().unwrap_or(0),
                start: note_on.tick(),
                end: note_off.tick(),
            });
            targets.push(on_idx);
            self.events.push(note_on);
            self.events.push(note_off);
        }

        self.resolve_overlaps(spans, &targets);
    }

    /// The selected notes for the note clipboard, earliest first, each placed
    /// relative to the earliest start — so a paste lands that note on the
    /// paste tick and the rest keep their spacing after it. Empty with
    /// nothing selected.
    pub(crate) fn copy_selected_notes(&self) -> Vec<CopiedNote> {
        let mut on_indices = self.note_on_indices(self.event_selection.ids());
        on_indices.sort_by_key(|&on_idx| (self.events[on_idx].tick(), on_idx));
        let Some(first_start) = on_indices.first().map(|&on_idx| self.events[on_idx].tick()) else {
            return Vec::new();
        };

        let pairing = self.pair_note_events();
        on_indices
            .into_iter()
            .map(|on_idx| {
                let note_on = &self.events[on_idx];
                CopiedNote {
                    offset: note_on.tick() - first_start,
                    length: pairing.note_end_tick(&self.events, on_idx) - note_on.tick(),
                    midi_message: note_on.midi_message().to_vec(),
                    muted: note_on.is_muted(),
                }
            })
            .collect()
    }

    /// `⌘/Ctrl+D` in the clip view: copies of the selected notes, pasted
    /// ([`pasted_notes`](Self::pasted_notes)) flush after the selection —
    /// the earliest copy at the latest selected end. Empty with nothing
    /// selected.
    pub(crate) fn duplicated_notes(&self) -> Vec<(Event, Event)> {
        let copied = self.copy_selected_notes();
        let first_start = self
            .note_on_indices(self.event_selection.ids())
            .iter()
            .map(|&on_idx| self.events[on_idx].tick())
            .min();
        let span = copied.iter().map(|note| note.offset + note.length).max();
        let (Some(first_start), Some(span)) = (first_start, span) else {
            return Vec::new();
        };
        self.pasted_notes(first_start + span, &copied)
    }

    /// New note pairs for [`insert_notes`](Self::insert_notes): `notes` with
    /// the first landing at event tick `tick`. Only notes that start inside
    /// the window are kept, and their ends stop at the window end — a paste
    /// never extends the clip or hides notes outside it. Fresh ids.
    pub(crate) fn pasted_notes(&self, tick: i32, notes: &[CopiedNote]) -> Vec<(Event, Event)> {
        let (window_start, window_end) = (self.region.start(), self.region.end());
        notes
            .iter()
            .filter_map(|note| {
                let start = tick + note.offset;
                if start < window_start || start >= window_end {
                    return None;
                }
                let (mut note_on, mut note_off) =
                    self.note_pair(start, note.length, note.midi_message.clone());
                note_on.set_muted(note.muted);
                note_off.set_muted(note.muted);
                Some((note_on, note_off))
            })
            .collect()
    }

    /// Applies `drag` to the notes in `event_ids` (their `NoteOn` ids; others
    /// are skipped), clamped by [`NoteDrag::dragged_spans`], then resolves
    /// same-pitch overlaps with them.
    pub(crate) fn apply_note_drag(&mut self, event_ids: &[Uuid], drag: NoteDrag) {
        let targets = self.note_on_indices(event_ids);
        if targets.is_empty() {
            return;
        }

        let pairing = self.pair_note_events();
        let before: Vec<NoteBounds> = targets
            .iter()
            .map(|&on_idx| {
                let on = &self.events[on_idx];
                (
                    on.tick(),
                    pairing.note_end_tick(&self.events, on_idx),
                    on.note_number().unwrap_or(0),
                )
            })
            .collect();
        let after = drag.dragged_spans(&before, (self.region.start(), self.region.end()));

        for ((&on_idx, &(start, end, pitch)), &(new_start, new_end, new_pitch)) in
            targets.iter().zip(&before).zip(&after)
        {
            match drag {
                NoteDrag::Move { .. } => {
                    self.nudge_note_pair(on_idx, new_start - start, &pairing);
                    if new_pitch != pitch {
                        let semitones = i32::from(new_pitch) - i32::from(pitch);
                        self.transpose_note_pair(on_idx, semitones, &pairing);
                    }
                }
                NoteDrag::ResizeStart { .. } => {
                    self.events[on_idx].set_tick(new_start);
                    self.events[on_idx].set_length(end - new_start);
                }
                NoteDrag::ResizeEnd { .. } => {
                    let off_idx = pairing.on_to_off.get(&on_idx).copied();
                    self.set_note_end(on_idx, off_idx, new_end);
                }
            }
        }

        let spans = self.note_spans(&pairing);
        self.resolve_overlaps(spans, &targets);
    }

    /// Every note in the list as a [`NoteSpan`], from a pairing taken before
    /// any tick moved (so the indices still match) but reading current ticks.
    fn note_spans(&self, pairing: &NotePairing) -> Vec<NoteSpan> {
        self.events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.event_type() == Some(EventType::NoteOn))
            .map(|(on_idx, event)| NoteSpan {
                on_idx,
                off_idx: pairing.on_to_off.get(&on_idx).copied(),
                pitch: event.note_number().unwrap_or(0),
                start: event.tick(),
                end: pairing.note_end_tick(&self.events, on_idx),
            })
            .collect()
    }

    /// Trims or removes notes so no note in `targets` (`NoteOn` indices)
    /// overlaps another note of its pitch (see the module docs), then
    /// re-sorts via [`sort_events_by_tick`](Clip::sort_events_by_tick).
    fn resolve_overlaps(&mut self, spans: Vec<NoteSpan>, targets: &[usize]) {
        let targets: HashSet<usize> = targets.iter().copied().collect();
        let pitches: HashSet<u8> = spans
            .iter()
            .filter(|span| targets.contains(&span.on_idx))
            .map(|span| span.pitch)
            .collect();

        let mut removed: Vec<usize> = Vec::new();
        for pitch in pitches {
            let mut notes: Vec<NoteSpan> = spans
                .iter()
                .filter(|span| span.pitch == pitch)
                .copied()
                .collect();
            // On equal starts the edited note sorts last, so it is the one
            // kept below.
            notes.sort_by_key(|span| (span.start, targets.contains(&span.on_idx)));

            let mut kept: Option<NoteSpan> = None;
            for note in notes {
                let Some(earlier) = kept.as_mut() else {
                    kept = Some(note);
                    continue;
                };
                let involves_target =
                    targets.contains(&earlier.on_idx) || targets.contains(&note.on_idx);
                if involves_target && note.start < earlier.end {
                    if note.start == earlier.start {
                        removed.extend(
                            [Some(earlier.on_idx), earlier.off_idx]
                                .into_iter()
                                .flatten(),
                        );
                    } else {
                        earlier.end = note.start;
                        self.set_note_end(earlier.on_idx, earlier.off_idx, note.start);
                    }
                }
                if note.end > earlier.end || involves_target {
                    kept = Some(note);
                }
            }
        }

        removed.sort_unstable();
        for idx in removed.into_iter().rev() {
            self.events.remove(idx);
        }
        self.sort_events_by_tick();
    }
}

/// The smallest and largest of `values`, `None` when empty.
pub(crate) fn min_max<T: Ord + Copy>(values: impl IntoIterator<Item = T>) -> Option<(T, T)> {
    values.into_iter().fold(None, |acc, value| {
        Some(acc.map_or((value, value), |(lo, hi)| (lo.min(value), hi.max(value))))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clip with window `[0, 1920)` holding `notes` as `(start, end,
    /// pitch)`. Returns the clip and the `NoteOn` ids in `notes` order.
    fn clip_with(notes: &[(i32, i32, u8)]) -> (Clip, Vec<Uuid>) {
        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(1920));
        let mut ids = Vec::new();
        for &(start, end, pitch) in notes {
            let on = Event::new(start, end - start, vec![0x90, pitch, 100]);
            ids.push(on.id());
            clip.events.push(on);
            clip.events.push(Event::new(end, 0, vec![0x80, pitch, 0]));
        }
        clip.sort_events_by_tick();
        (clip, ids)
    }

    /// Every note as `(start, end, pitch)`, read back through a fresh
    /// pairing (so a mismatched `NoteOff` shows up), sorted.
    fn notes(clip: &Clip) -> Vec<(i32, i32, u8)> {
        let pairing = clip.pair_note_events();
        assert!(pairing.orphan_note_offs.is_empty(), "orphan NoteOff");
        assert!(pairing.open_note_ons.is_empty(), "open NoteOn");
        let mut notes: Vec<_> = clip
            .note_spans(&pairing)
            .iter()
            .map(|span| (span.start, span.end, span.pitch))
            .collect();
        notes.sort_unstable();
        notes
    }

    /// The length field of the `NoteOn` `id`.
    fn length_of(clip: &Clip, id: Uuid) -> i32 {
        let on = clip.events.iter().find(|e| e.id() == id).unwrap();
        on.end_tick() - on.tick()
    }

    #[test]
    fn insert_note_adds_a_paired_note() {
        let (mut clip, _) = clip_with(&[]);
        let (on, off) = clip.new_note(480, 480, 60, 100);
        clip.insert_notes([(on, off)]);
        assert_eq!(notes(&clip), vec![(480, 960, 60)]);
    }

    #[test]
    fn new_note_clamps_into_the_window_and_midi_ranges() {
        let (clip, _) = clip_with(&[]);
        let (on, off) = clip.new_note(1900, 480, 200, 0);
        assert_eq!((on.tick(), off.tick()), (1900, 1920));
        assert_eq!(on.note_number(), Some(127));
        assert_eq!(on.velocity(), Some(1));

        let (on, off) = clip.new_note(-50, 0, -3, 300);
        assert_eq!((on.tick(), off.tick()), (0, 1));
        assert_eq!(on.note_number(), Some(0));
        assert_eq!(on.velocity(), Some(127));
    }

    #[test]
    fn insert_note_trims_an_earlier_same_pitch_note() {
        let (mut clip, _) = clip_with(&[(0, 960, 60), (0, 960, 64)]);
        let (on, off) = clip.new_note(480, 480, 60, 100);
        clip.insert_notes([(on, off)]);
        assert_eq!(
            notes(&clip),
            vec![(0, 480, 60), (0, 960, 64), (480, 960, 60)]
        );
    }

    #[test]
    fn insert_note_is_trimmed_by_a_later_same_pitch_note() {
        let (mut clip, _) = clip_with(&[(480, 960, 60)]);
        let (on, off) = clip.new_note(0, 960, 60, 100);
        clip.insert_notes([(on, off)]);
        assert_eq!(notes(&clip), vec![(0, 480, 60), (480, 960, 60)]);
    }

    #[test]
    fn insert_note_replaces_a_same_pitch_note_with_the_same_start() {
        let (mut clip, _) = clip_with(&[(480, 500, 60)]);
        let (on, off) = clip.new_note(480, 480, 60, 100);
        let new_id = on.id();
        clip.insert_notes([(on, off)]);
        assert_eq!(notes(&clip), vec![(480, 960, 60)]);
        assert!(clip.events.iter().any(|e| e.id() == new_id));
    }

    #[test]
    fn copy_places_notes_relative_to_the_earliest_start() {
        let (mut clip, ids) = clip_with(&[(500, 600, 64), (0, 240, 60), (90, 960, 62)]);
        clip.restore_event_selection(&[ids[0], ids[2]]);
        let copied = clip.copy_selected_notes();
        let shape: Vec<_> = copied
            .iter()
            .map(|note| (note.offset, note.length, note.midi_message[1]))
            .collect();
        assert_eq!(shape, vec![(0, 870, 62), (410, 100, 64)]);
    }

    #[test]
    fn copy_with_nothing_selected_is_empty() {
        let (clip, _) = clip_with(&[(0, 240, 60)]);
        assert!(clip.copy_selected_notes().is_empty());
    }

    /// `notes` (as for [`clip_with`]) all selected and copied.
    fn copied(notes: &[(i32, i32, u8)]) -> Vec<CopiedNote> {
        let (mut source, ids) = clip_with(notes);
        source.restore_event_selection(&ids);
        source.copy_selected_notes()
    }

    #[test]
    fn paste_lands_the_earliest_note_on_the_tick_keeping_spacing() {
        let copied = copied(&[(10, 250, 60), (490, 730, 64), (970, 1210, 67)]);

        let (mut clip, _) = clip_with(&[]);
        let pasted = clip.pasted_notes(480, &copied);
        clip.insert_notes(pasted);
        assert_eq!(
            notes(&clip),
            vec![(480, 720, 60), (960, 1200, 64), (1440, 1680, 67)]
        );
    }

    #[test]
    fn paste_keeps_velocity_and_mute() {
        let (mut source, ids) = clip_with(&[(0, 240, 60)]);
        source.events[0].set_velocity(42);
        source.restore_event_selection(&ids);
        source.toggle_muted_for_selected_events();
        let copied = source.copy_selected_notes();

        let (clip, _) = clip_with(&[]);
        let pasted = clip.pasted_notes(960, &copied);
        let (on, off) = &pasted[0];
        assert_eq!(on.velocity(), Some(42));
        assert!(on.is_muted() && off.is_muted());
        assert_ne!(on.id(), source.events[0].id());
    }

    #[test]
    fn paste_drops_notes_past_the_window_and_trims_ends_at_it() {
        let copied = copied(&[(0, 480, 60), (480, 960, 62), (960, 1440, 64)]);

        let (mut clip, _) = clip_with(&[]);
        let pasted = clip.pasted_notes(1200, &copied);
        clip.insert_notes(pasted);
        assert_eq!(notes(&clip), vec![(1200, 1680, 60), (1680, 1920, 62)]);

        assert!(clip.pasted_notes(1920, &copied).is_empty());
    }

    #[test]
    fn paste_trims_or_replaces_same_pitch_notes_it_lands_on() {
        let copied = copied(&[(0, 480, 60), (480, 960, 62)]);

        let (mut clip, _) = clip_with(&[(0, 960, 60), (480, 600, 62)]);
        let pasted = clip.pasted_notes(0, &copied);
        clip.insert_notes(pasted);
        assert_eq!(notes(&clip), vec![(0, 480, 60), (480, 960, 62)]);
    }

    #[test]
    fn duplicate_lays_copies_flush_after_the_latest_selected_end() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (480, 960, 61), (1500, 1600, 62)]);
        clip.restore_event_selection(&ids[..2]);
        let duplicated = clip.duplicated_notes();
        assert!(duplicated.iter().all(|(on, _)| !ids.contains(&on.id())));
        clip.insert_notes(duplicated);
        assert_eq!(
            notes(&clip),
            vec![
                (0, 240, 60),
                (480, 960, 61),
                (960, 1200, 60),
                (1440, 1920, 61),
                (1500, 1600, 62),
            ]
        );
    }

    #[test]
    fn duplicate_with_nothing_selected_is_empty() {
        let (clip, _) = clip_with(&[(0, 240, 60)]);
        assert!(clip.duplicated_notes().is_empty());
    }

    #[test]
    fn move_shifts_time_and_pitch_keeping_length() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (480, 720, 64)]);
        clip.apply_note_drag(
            &ids,
            NoteDrag::Move {
                delta_ticks: 240,
                delta_pitch: 2,
            },
        );
        assert_eq!(notes(&clip), vec![(240, 480, 62), (720, 960, 66)]);
    }

    #[test]
    fn move_clamps_the_group_to_the_window_and_pitch_range() {
        let (mut clip, ids) = clip_with(&[(240, 480, 120), (960, 1200, 60)]);
        clip.apply_note_drag(
            &ids,
            NoteDrag::Move {
                delta_ticks: -1000,
                delta_pitch: 20,
            },
        );
        // The earlier note stops at the window start, the higher one at 127.
        assert_eq!(notes(&clip), vec![(0, 240, 127), (720, 960, 67)]);
    }

    #[test]
    fn move_never_pushes_a_note_outside_the_window_further_out() {
        let (mut clip, ids) = clip_with(&[(2000, 2100, 60)]);
        clip.apply_note_drag(
            &ids,
            NoteDrag::Move {
                delta_ticks: 100,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip), vec![(2000, 2100, 60)]);
    }

    #[test]
    fn move_only_touches_the_targets() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (0, 240, 64)]);
        clip.apply_note_drag(
            &ids[..1],
            NoteDrag::Move {
                delta_ticks: 480,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip), vec![(0, 240, 64), (480, 720, 60)]);
    }

    #[test]
    fn move_across_a_same_pitch_note_keeps_every_note_paired() {
        // Moving the first note to start inside the second: the second (now
        // the earlier one) is trimmed to the moved note's start.
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (480, 960, 60)]);
        clip.apply_note_drag(
            &ids[..1],
            NoteDrag::Move {
                delta_ticks: 720,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip), vec![(480, 720, 60), (720, 960, 60)]);
        assert_eq!(length_of(&clip, ids[1]), 240);
    }

    #[test]
    fn move_onto_a_same_pitch_start_removes_the_other_note() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (480, 960, 60)]);
        clip.apply_note_drag(
            &ids[..1],
            NoteDrag::Move {
                delta_ticks: 480,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip), vec![(480, 720, 60)]);
        assert!(clip.events.iter().any(|e| e.id() == ids[0]));
    }

    #[test]
    fn move_leaves_overlaps_between_untouched_notes_alone() {
        let (mut clip, ids) = clip_with(&[(0, 960, 60), (480, 600, 60), (0, 240, 64)]);
        // The two pitch-60 notes overlap already; moving the pitch-64 one
        // must not touch them.
        let before = notes(&clip).len();
        clip.apply_note_drag(
            &ids[2..],
            NoteDrag::Move {
                delta_ticks: 240,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip).len(), before);
    }

    #[test]
    fn resize_end_grows_and_shrinks_with_clamps() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (1800, 1900, 64)]);
        clip.apply_note_drag(&ids, NoteDrag::ResizeEnd { delta_ticks: 240 });
        // The second stops at the window end.
        assert_eq!(notes(&clip), vec![(0, 480, 60), (1800, 1920, 64)]);
        assert_eq!(length_of(&clip, ids[0]), 480);

        clip.apply_note_drag(&ids, NoteDrag::ResizeEnd { delta_ticks: -1000 });
        assert_eq!(notes(&clip), vec![(0, 1, 60), (1800, 1801, 64)]);
    }

    #[test]
    fn resize_end_into_a_later_same_pitch_note_stops_at_its_start() {
        let (mut clip, ids) = clip_with(&[(0, 240, 60), (480, 720, 60)]);
        clip.apply_note_drag(&ids[..1], NoteDrag::ResizeEnd { delta_ticks: 600 });
        assert_eq!(notes(&clip), vec![(0, 480, 60), (480, 720, 60)]);
    }

    #[test]
    fn resize_start_keeps_the_end_with_clamps() {
        let (mut clip, ids) = clip_with(&[(240, 480, 60)]);
        clip.apply_note_drag(&ids, NoteDrag::ResizeStart { delta_ticks: -120 });
        assert_eq!(notes(&clip), vec![(120, 480, 60)]);
        assert_eq!(length_of(&clip, ids[0]), 360);

        clip.apply_note_drag(&ids, NoteDrag::ResizeStart { delta_ticks: -1000 });
        assert_eq!(notes(&clip), vec![(0, 480, 60)]);

        clip.apply_note_drag(&ids, NoteDrag::ResizeStart { delta_ticks: 1000 });
        assert_eq!(notes(&clip), vec![(479, 480, 60)]);
    }

    #[test]
    fn resize_start_over_an_earlier_same_pitch_note_trims_it() {
        let (mut clip, ids) = clip_with(&[(0, 480, 60), (480, 720, 60)]);
        clip.apply_note_drag(&ids[1..], NoteDrag::ResizeStart { delta_ticks: -240 });
        assert_eq!(notes(&clip), vec![(0, 240, 60), (240, 720, 60)]);
    }

    /// The preview rule is the commit rule: `dragged_spans` predicts exactly
    /// where `apply_note_drag` puts the notes, clamps included.
    #[test]
    fn dragged_spans_match_what_apply_note_drag_commits() {
        let spans = [(240, 480, 120), (1800, 1900, 60)];
        for drag in [
            NoteDrag::Move {
                delta_ticks: -1000,
                delta_pitch: 20,
            },
            NoteDrag::ResizeStart { delta_ticks: -500 },
            NoteDrag::ResizeEnd { delta_ticks: 240 },
        ] {
            let (mut clip, ids) = clip_with(&spans);
            clip.apply_note_drag(&ids, drag);
            let mut predicted = drag.dragged_spans(&spans, (0, 1920));
            predicted.sort_unstable();
            assert_eq!(notes(&clip), predicted, "{drag:?}");
        }
    }

    #[test]
    fn with_deltas_keeps_the_kind_of_drag() {
        let moved = NoteDrag::Move {
            delta_ticks: 0,
            delta_pitch: 0,
        };
        assert_eq!(
            moved.with_deltas(240, 2),
            NoteDrag::Move {
                delta_ticks: 240,
                delta_pitch: 2,
            }
        );
        assert_eq!(
            NoteDrag::ResizeEnd { delta_ticks: 0 }.with_deltas(-240, 5),
            NoteDrag::ResizeEnd { delta_ticks: -240 }
        );
    }

    #[test]
    fn note_drag_is_noop_only_with_every_delta_zero() {
        let still = NoteDrag::Move {
            delta_ticks: 0,
            delta_pitch: 0,
        };
        let pitch_only = NoteDrag::Move {
            delta_ticks: 0,
            delta_pitch: 1,
        };
        assert!(still.is_noop());
        assert!(!pitch_only.is_noop());
        assert!(NoteDrag::ResizeEnd { delta_ticks: 0 }.is_noop());
        assert!(!NoteDrag::ResizeStart { delta_ticks: -1 }.is_noop());
    }

    #[test]
    fn unknown_ids_are_a_no_op() {
        let (mut clip, _) = clip_with(&[(0, 240, 60)]);
        clip.apply_note_drag(
            &[Uuid::new_v4()],
            NoteDrag::Move {
                delta_ticks: 240,
                delta_pitch: 0,
            },
        );
        assert_eq!(notes(&clip), vec![(0, 240, 60)]);
    }
}
