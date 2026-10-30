//! The take session (`archive/290-framing-a-take.md` § Phase 2): the stopped `/` with
//! a lead clip opens the take as its own clip, off the arrangement, and the
//! user frames the notes to insert before they land. While it is open
//! [`selected_clip`](Sequencer::selected_clip) is the take, so the clip view
//! and its cursor commands act on it unchanged, and the target clip already
//! holds the framed notes at its clip cursor, so the ordinary transport plays
//! them in place. Those notes are temporary: Enter puts the target back as it
//! was and hands its `InsertCaptureEdit` the framed notes, Esc just puts it
//! back. Neither the session nor its window moves are undo steps.
//!
//! The take's window moves like any clip's (`[`/`]` at the cursor, the band
//! on the grid: `Sequencer::selected_clip_marker_at`, which sees the take as
//! the selected clip), and selects whole notes by where they
//! start: a note starting inside it is in, kept whole past its end, and so
//! is one starting up to [`TAKE_EARLY_TICKS`] before it — a note played a
//! little ahead of the beat `[` sits on.

use uuid::Uuid;

use crate::core::{time::PPQN, view_state::ViewState};
use crate::models::{
    clip::{Clip, ClipEdge},
    event::{Event, EventType},
};

use super::{CaptureInsert, EdgeTarget, Sequencer};

/// How far before the window start a note may start and still be in the
/// take — a 32nd, so a chord played just ahead of the beat `[` is on comes
/// along. It lands as far before the insert point as it was played.
pub(crate) const TAKE_EARLY_TICKS: i32 = PPQN / 8;

/// An open take session. See the module docs.
pub(crate) struct TakeFraming {
    /// The take: the capture source as a clip, its window the framed notes,
    /// its arrangement start the insert point (so the playhead maps onto it
    /// where the take is heard). Never in the arrangement.
    take: Clip,
    /// Track the target clip is on.
    track_idx: usize,
    /// The clip the take goes into.
    clip_id: Uuid,
    /// The target's clip cursor when the session opened, in its event ticks:
    /// where the window start lands. Nothing moves it while the session is
    /// open; it is kept so nothing has to.
    insert_at: i32,
    /// The target's window when the session opened, in its event ticks: a
    /// framed note starting outside it is dropped, one running past its end
    /// is closed there.
    target_window: (i32, i32),
    /// The target's events from before the session, put back on Esc and
    /// before Enter's edit snapshots the clip.
    pre_events: Vec<Event>,
    /// The view that had the keyboard when the session opened, which gets
    /// it back when it closes.
    return_to: ViewState,
}

impl TakeFraming {
    /// The framed take as it lands in the target, in its event ticks: the
    /// notes starting in the take's window (or up to [`TAKE_EARLY_TICKS`]
    /// before it), whole, and the wheel moves from the window start to the
    /// [`wheel_end`](Self::wheel_end) — a wheel off its rest at the start
    /// counted as a move there — shifted so the window start is at the
    /// insert point. A note starting outside the target's window is dropped,
    /// one running past its end is closed there; the wheel span never runs
    /// past it ([`Clip::land_take`]).
    fn framed_events(&self) -> CaptureInsert {
        let (start, end) = (self.take.region().start(), self.take.region().end());
        let notes: Vec<Event> = self
            .take
            .cloned_events_in_range(start - TAKE_EARLY_TICKS, end)
            .into_iter()
            .filter(Event::is_note_edge)
            .collect();
        let last_off = notes.iter().map(Event::tick).fold(end, i32::max);
        let wheel_end = self.wheel_end(last_off);
        let carried = self.take.carried_wheel_moves(start);
        let wheels: Vec<Event> = self
            .take
            .events()
            .iter()
            .filter(|event| !event.is_note_edge())
            .filter(|event| (start..wheel_end).contains(&event.tick()))
            .cloned()
            .chain(carried)
            .collect();

        let shift = self.insert_at - start;
        let mut phrase = Clip::new();
        phrase.restore_events(
            notes
                .into_iter()
                .chain(wheels)
                .map(|mut event| {
                    event.set_tick(event.tick() + shift);
                    event
                })
                .collect(),
        );
        let (target_start, target_end) = self.target_window;
        let length = target_end - target_start;
        phrase
            .region_mut()
            .set_region(Some(target_start), Some(target_end));
        phrase.calculate_note_lengths();
        phrase.crop(length);
        CaptureInsert {
            track_idx: self.track_idx,
            clip_id: self.clip_id,
            events: Sequencer::capture_events_to_insert(&phrase, target_start, length),
            wheel_end: wheel_end + shift,
        }
    }

    /// Where the take's wheel span ends, in take ticks, for a take whose
    /// notes end at `last_off` (`370-wheel-capture.md` § Decisions 10): a
    /// bend easing back during the release belongs to the phrase, so the
    /// span runs on to the take's next note-on after `last_off`; with none,
    /// just past its last wheel move from `last_off` on; with none of those
    /// either, to `last_off` itself.
    fn wheel_end(&self, last_off: i32) -> i32 {
        let events = self.take.events();
        let next_on = events
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(Event::tick)
            .filter(|&tick| tick > last_off)
            .min();
        next_on.unwrap_or_else(|| {
            events
                .iter()
                .filter(|event| !event.is_note_edge() && event.tick() >= last_off)
                .map(|event| event.tick() + 1)
                .fold(last_off, i32::max)
        })
    }
}

impl Sequencer {
    /// Whether a take session is open.
    pub(crate) fn is_framing_take(&self) -> bool {
        self.take_framing.is_some()
    }

    /// The events a save writes for `clip`: the target of an open session as
    /// it was before the session (the take is no part of the project until
    /// Enter inserts it), any other clip as it is.
    pub(crate) fn saved_events<'a>(&'a self, clip: &'a Clip) -> &'a [Event] {
        match &self.take_framing {
            Some(framing) if framing.clip_id == clip.id() => &framing.pre_events,
            _ => clip.events(),
        }
    }

    /// The take of the open session, which [`selected_clip`](Self::selected_clip)
    /// returns while it is open.
    pub(super) fn framed_take(&self) -> Option<&Clip> {
        self.take_framing.as_ref().map(|framing| &framing.take)
    }

    /// Mutable access to the open session's take. (Checked first by
    /// `selected_clip_mut`: returning a borrow out of an `if let` on `self`
    /// would keep `self` borrowed on the fallback path.)
    pub(super) fn framed_take_mut(&mut self) -> Option<&mut Clip> {
        self.take_framing.as_mut().map(|framing| &mut framing.take)
    }

    /// The stopped `/` with a lead clip: opens a take session on the capture
    /// buffer, framed at detection's phrase start through the last note-off,
    /// and puts the framed notes into the lead clip at its clip cursor.
    /// `return_to` is the view that has the keyboard now. `false` (and
    /// nothing opens) on a buffer without notes, no lead clip, a session
    /// already open, or no room at the cursor.
    pub(crate) fn open_take_framing(&mut self, return_to: ViewState) -> bool {
        if self.take_framing.is_some() {
            return false;
        }
        let Some(framing) = self.build_take_framing(return_to) else {
            return false;
        };
        let framed = framing.framed_events();
        if framed.events.is_empty() {
            return false;
        }
        self.take_framing = Some(framing);
        self.hear_take_in_place(framed);
        true
    }

    /// The session [`open_take_framing`](Self::open_take_framing) opens: the
    /// clip the arranger's stopped `/` would make (the capture source, its
    /// window starting where detection puts the phrase), except that the
    /// window ends at the last note-off, with no tail.
    fn build_take_framing(&self, return_to: ViewState) -> Option<TakeFraming> {
        let track_idx = self.selected_track_index()?;
        let target = self.selected_clip()?;
        let insert_at = target.cursor_tick();
        let target_window = (target.region().start(), target.region().end());
        if insert_at >= target_window.1 || !self.capture_clip.has_notes() {
            return None;
        }

        let capture = self.detection_capture_source();
        let (start, _) =
            Self::detected_phrase_window(&capture, self.loop_reference_length(), self.meter());
        let end = Self::max_event_tick(capture.events(), EventType::NoteOff);

        let mut take = Self::framed(capture, start, end.max(start + 1));
        take.set_start_tick(target.arrangement_tick_from_event_tick(insert_at));
        take.nudge_cursor_to_region_start();
        Some(TakeFraming {
            take,
            track_idx,
            clip_id: target.id(),
            insert_at,
            target_window,
            pre_events: target.events().to_vec(),
            return_to,
        })
    }

    /// `[`/`]` in the session: the take's window `edge` to its cursor, by
    /// the clip view's marker rule (`clip_edge_target`). `false` when
    /// nothing changed.
    pub(crate) fn set_take_edge_to_cursor(&mut self, edge: ClipEdge) -> bool {
        let target = self.clip_edge_target(edge, true);
        self.move_take_window(target)
    }

    /// The window band released in the session: the take's window `edge`
    /// to the event tick `tick`, by the marker rule its preview drew
    /// (`selected_clip_marker_at`). `false` when nothing changed.
    pub(crate) fn set_take_edge_at(&mut self, edge: ClipEdge, tick: i32) -> bool {
        let target = self.selected_clip_marker_at(edge, tick);
        self.move_take_window(target)
    }

    /// Moves the take to `target`'s bounds — a marker edit worked out on
    /// the take, which is the selected clip — directly, not as a
    /// `ResizeClipEdit`: the take isn't in the arrangement. The lead clip
    /// hears the new framing.
    fn move_take_window(&mut self, target: Option<EdgeTarget>) -> bool {
        let (Some(EdgeTarget { bounds, .. }), Some(framing)) = (target, &mut self.take_framing)
        else {
            return false;
        };
        framing.take.set_bounds(bounds);
        let framed = framing.framed_events();
        self.hear_take_in_place(framed);
        true
    }

    /// Esc (and everything else that ends a session without inserting): puts
    /// the target's events back as they were and closes the session. The
    /// capture buffer is left alone, so `/` reopens the take. The view to
    /// hand the keyboard back to, `None` with no session open.
    pub(crate) fn cancel_take_framing(&mut self) -> Option<ViewState> {
        self.close_take_framing().map(|framing| framing.return_to)
    }

    /// Enter: closes the session, the target's events put back as they were,
    /// and returns the view to hand the keyboard back to and the framed take
    /// for `InsertCaptureEdit` to land, so its undo state is the clip from
    /// before the session — `None` for a window that frames no note. `None`
    /// with no session open.
    pub(crate) fn finish_take_framing(&mut self) -> Option<(ViewState, Option<CaptureInsert>)> {
        let framing = self.close_take_framing()?;
        let insert = framing.framed_events();
        let insert = (!insert.events.is_empty()).then_some(insert);
        Some((framing.return_to, insert))
    }

    /// Closes the session and puts the target's events back as they were
    /// before it. The closed session, `None` with none open.
    fn close_take_framing(&mut self) -> Option<TakeFraming> {
        let framing = self.take_framing.take()?;
        let pre_events = framing.pre_events.clone();
        self.edit_clip_events(framing.track_idx, framing.clip_id, |clip| {
            clip.restore_events(pre_events);
            clip.calculate_note_lengths();
        });
        Some(framing)
    }

    /// Swaps the target's temporary take for `framed`, the session's framed
    /// take: its events from before the session with it landed on them, as
    /// Enter will land it, through `edit_clip_events`, so nothing hangs
    /// under a running playhead. The current pass plays out; the next loop
    /// wrap plays the change.
    fn hear_take_in_place(&mut self, framed: CaptureInsert) {
        let Some(framing) = &self.take_framing else {
            return;
        };
        let (track_idx, clip_id) = (framing.track_idx, framing.clip_id);
        let pre_events = framing.pre_events.clone();
        self.edit_clip_events(track_idx, clip_id, |clip| {
            clip.restore_events(pre_events);
            clip.land_take(&framed.events, framed.wheel_end);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use rtrb::Consumer;
    use undo::Record;

    use super::*;
    use crate::core::{
        project::ProjectData,
        sequencer::{
            ClipInstrumentEvent, EditResult, InsertCaptureEdit, SequencerEdit,
            test_support::{
                bend_at, clip_at, instrument_track_0, note_off, note_on, sequencer_with,
                tick_and_collect,
            },
        },
        time::Meter,
        view_state::ViewState,
    };
    use crate::models::event::EventType;

    const BAR: i32 = Meter::FOUR_FOUR.bar_ticks();

    /// Track 0 selected, a two-bar loop region, a lead clip of `clip_len`
    /// at bar 5 with its clip cursor at `clip_cursor`, and `notes` (`(on,
    /// off, pitch)` each) in the capture buffer.
    fn session_seq(
        clip_len: i32,
        clip_cursor: i32,
        notes: &[(i32, i32, u8)],
    ) -> (Sequencer, Consumer<ClipInstrumentEvent>) {
        let (mut seq, rx) = sequencer_with(true);
        seq.set_global_region(0, BAR * 2);
        let track_id = seq.track_id_by_index(0).unwrap();
        seq.select_track(Some(track_id));
        let clip = clip_at(BAR * 4, clip_len);
        seq.tracks_mut()[0].add_clip(&clip);
        seq.select_clip(Some(clip.id()));
        seq.selected_clip_mut()
            .unwrap()
            .set_cursor_tick(clip_cursor);
        capture(&mut seq, notes);
        (seq, rx)
    }

    /// Adds `notes` (`(on, off, pitch)` each) to the capture buffer.
    fn capture(seq: &mut Sequencer, notes: &[(i32, i32, u8)]) {
        let mut events: Vec<_> = notes
            .iter()
            .flat_map(|&(on, off, pitch)| {
                [
                    Event::new(on, 0, vec![0x90, pitch, 100]),
                    Event::new(off, 0, vec![0x80, pitch, 0]),
                ]
            })
            .collect();
        events.sort_by_key(Event::tick);
        for event in events {
            seq.capture_clip.add_event(event);
        }
    }

    /// The lead clip's notes as `(on, off, pitch)`, sorted — the clip in the
    /// arrangement, not the take.
    fn lead_notes(seq: &Sequencer) -> Vec<(i32, i32, u8)> {
        let mut notes: Vec<_> = seq
            .lead_clip()
            .unwrap()
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| (e.tick(), e.end_tick(), e.note_number().unwrap()))
            .collect();
        notes.sort_unstable();
        notes
    }

    /// Enter: the session's insert recorded on `record`, and its result.
    fn enter(seq: &mut Sequencer, record: &mut Record<SequencerEdit>) -> EditResult {
        let (_, insert) = seq.finish_take_framing().expect("a session to insert");
        let edit = InsertCaptureEdit::from_insert(seq, insert.expect("framed notes"))
            .expect("the target clip");
        record.edit(seq, SequencerEdit::InsertCapture(edit))
    }

    /// The pitch range an insert's result reports for the piano roll to
    /// bring into view (`EditResult::EventsModified::inserted_take`).
    fn inserted_take(result: EditResult) -> Option<(u8, u8)> {
        match result {
            EditResult::EventsModified { inserted_take, .. } => inserted_take,
            _ => panic!("an events edit"),
        }
    }

    /// `/` then Enter inserts the whole last phrase at the clip cursor, its
    /// notes whole (`stopped_insert_lands_the_detected_phrase_at_the_clip_cursor`
    /// before the session, which kept at most a bar and closed the last note
    /// a tick early); an earlier phrase is left out, and a note running past
    /// the clip's end is closed there.
    #[test]
    fn enter_inserts_the_whole_last_phrase_at_the_clip_cursor() {
        let after_enter = |clip_cursor: i32, notes: &[(i32, i32, u8)]| {
            let (mut seq, _) = session_seq(BAR * 4, clip_cursor, notes);
            assert!(seq.open_take_framing(ViewState::Clip));
            enter(&mut seq, &mut Record::new());
            assert!(!seq.is_framing_take());
            assert!(seq.capture_clip.events().is_empty(), "buffer consumed");
            lead_notes(&seq)
        };

        // Six notes a quarter apart over a bar and a half: all of them.
        let loose: Vec<_> = (0..6).map(|i| (500 + i * 960, 980 + i * 960, 60)).collect();
        let expected: Vec<_> = (0..6)
            .map(|i| (BAR + i * 960, BAR + i * 960 + 480, 60))
            .collect();
        assert_eq!(after_enter(BAR, &loose), expected);

        // 600 ticks before the clip end: the first note whole, the next one
        // starts past the end.
        let near_end = BAR * 4 - 600;
        assert_eq!(
            after_enter(near_end, &loose),
            vec![(near_end, near_end + 480, 60)]
        );
        // 300 ticks before it: the first note closed at the last tick.
        let nearer = BAR * 4 - 300;
        assert_eq!(after_enter(nearer, &loose), vec![(nearer, BAR * 4 - 1, 60)]);

        // An early note, silence, then a four-note phrase: the phrase only.
        let mut two_phrases = vec![(100, 400, 60)];
        two_phrases.extend((0..4).map(|i| (BAR * 3 + 300 + i * 960, BAR * 3 + 780 + i * 960, 62)));
        let expected: Vec<_> = (0..4).map(|i| (300 + i * 960, 780 + i * 960, 62)).collect();
        assert_eq!(after_enter(300, &two_phrases), expected);
    }

    /// The take's cursor to `tick`.
    fn take_cursor_to(seq: &mut Sequencer, tick: i32) {
        seq.selected_clip_mut().unwrap().set_cursor_tick(tick);
    }

    /// The window moves like a clip's, `[`/`]` at the cursor, and selects
    /// whole notes by where they start: one that started before `[` and
    /// still sounds is left out, one starting inside is kept whole past `]`,
    /// and one starting at `]` is out. The window start lands on the clip
    /// cursor.
    #[test]
    fn the_window_selects_whole_notes_by_their_start() {
        let notes = [
            (0, 1500, 60),
            (960, 3000, 62),
            (1920, 2400, 64),
            (3840, 4000, 65),
        ];
        let (mut seq, _) = session_seq(BAR * 4, 480, &notes);
        assert!(seq.open_take_framing(ViewState::Clip));

        take_cursor_to(&mut seq, 960);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::Start));
        take_cursor_to(&mut seq, 1920);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::End));
        assert_eq!(lead_notes(&seq), vec![(480, 480 + 2040, 62)]);
        let take = seq.selected_clip().unwrap();
        assert_eq!((take.region().start(), take.region().end()), (960, 1920));
    }

    /// A note played up to a 32nd ahead of the window start comes along,
    /// landing as far ahead of the clip cursor; one played earlier doesn't.
    /// The band moves the window by the same rule as the keys.
    #[test]
    fn a_note_played_just_before_the_start_comes_along() {
        let notes = [(960, 1200, 62), (1440, 1680, 64)];
        let (mut seq, _) = session_seq(BAR * 4, 480, &notes);
        assert!(seq.open_take_framing(ViewState::Clip));

        assert!(seq.set_take_edge_at(ClipEdge::Start, 960 + 40));
        assert_eq!(lead_notes(&seq), vec![(440, 680, 62), (920, 1160, 64)]);
        let late = 960 + TAKE_EARLY_TICKS + 1;
        assert!(seq.set_take_edge_at(ClipEdge::Start, late));
        let at = 480 + 1440 - late;
        assert_eq!(lead_notes(&seq), vec![(at, at + 240, 64)]);
    }

    /// The start can be moved up to the first of the last two notes even
    /// when they span less than a beat: a take's window is only held a tick
    /// apart, not a clip's beat (`min_window_length`). Before, the start
    /// stopped a beat before the end and the earlier notes came along
    /// (found 2026-10-27).
    #[test]
    fn the_start_frames_a_phrase_shorter_than_a_beat() {
        let notes = [
            (0, 200, 60),
            (480, 700, 62),
            (1000, 1200, 64),
            (1250, 1300, 65),
            (1400, 1500, 67),
        ];
        let (mut seq, _) = session_seq(BAR * 4, 480, &notes);
        assert!(seq.open_take_framing(ViewState::Clip));

        assert!(seq.set_take_edge_at(ClipEdge::Start, 1250));
        let take = seq.selected_clip().unwrap();
        assert_eq!((take.region().start(), take.region().end()), (1250, 1500));
        assert_eq!(lead_notes(&seq), vec![(480, 530, 65), (630, 730, 67)]);

        take_cursor_to(&mut seq, 1400);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::Start), "`[` too");
        assert_eq!(lead_notes(&seq), vec![(480, 580, 67)]);
    }

    /// Enter with a window that frames no note has nothing to insert, and
    /// closes the session as Esc would.
    #[test]
    fn enter_on_an_empty_window_inserts_nothing() {
        let notes = [(0, 240, 60), (1920, 2160, 62)];
        let (mut seq, _) = session_seq(BAR * 4, 0, &notes);
        assert!(seq.open_take_framing(ViewState::Clip));
        assert!(seq.set_take_edge_at(ClipEdge::Start, 480));
        assert!(seq.set_take_edge_at(ClipEdge::End, 960));
        assert_eq!(lead_notes(&seq), vec![]);

        assert!(matches!(seq.finish_take_framing(), Some((_, None))));
        assert!(!seq.is_framing_take());
        assert!(seq.capture_clip.has_notes(), "buffer kept");
    }

    /// While framing, the lead clip holds the framed notes beside its own,
    /// following every window change; Esc puts it back as it was, keeps its
    /// selection, and leaves the buffer for `/` to reopen.
    #[test]
    fn the_take_is_heard_in_place_and_esc_puts_the_clip_back() {
        let notes = [(0, 240, 60), (960, 1200, 62)];
        let (mut seq, _) = session_seq(BAR * 4, BAR, &notes);
        let existing = note_on(BAR * 3);
        let existing_id = existing.id();
        {
            let clip = seq.selected_clip_mut().unwrap();
            clip.add_event(existing);
            clip.add_event(note_off(BAR * 3 + 240));
            clip.calculate_note_lengths();
            clip.select_event(Some(existing_id));
        }
        let before = lead_notes(&seq);
        let buffer = seq.capture_clip.events().len();

        assert!(seq.open_take_framing(ViewState::Clip));
        assert_eq!(
            lead_notes(&seq),
            vec![
                (BAR, BAR + 240, 60),
                (BAR + 960, BAR + 1200, 62),
                (BAR * 3, BAR * 3 + 240, 60),
            ]
        );
        // `]` between the notes: the second is gone from the clip.
        take_cursor_to(&mut seq, 480);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::End));
        assert_eq!(
            lead_notes(&seq),
            vec![(BAR, BAR + 240, 60), (BAR * 3, BAR * 3 + 240, 60)]
        );

        assert_eq!(seq.cancel_take_framing(), Some(ViewState::Clip));
        assert!(!seq.is_framing_take());
        assert_eq!(lead_notes(&seq), before);
        assert_eq!(seq.capture_clip.events().len(), buffer, "buffer kept");
        assert_eq!(
            seq.selected_clip().unwrap().selected_event_ids(),
            vec![existing_id]
        );
        // `/` reopens the take.
        assert!(seq.open_take_framing(ViewState::Clip));
    }

    /// A save during a session writes the target as it was before it: the
    /// temporary notes the take is heard by are no part of the project.
    #[test]
    fn a_save_leaves_the_take_out() {
        let notes = [(0, 240, 60), (960, 1200, 62)];
        let (mut seq, _) = session_seq(BAR * 4, BAR, &notes);
        let before = ProjectData::from_sequencer(&seq).fingerprint();

        assert!(seq.open_take_framing(ViewState::Clip));
        assert_eq!(lead_notes(&seq).len(), 2, "the take is heard in place");
        assert_eq!(ProjectData::from_sequencer(&seq).fingerprint(), before);
    }

    /// Enter is one undo step whose undo is the clip from before the
    /// session, not the clip with the temporary notes; the selection is
    /// never changed. The insert and its redo report the take's pitch range,
    /// so the piano roll re-frames over it either way; the undo none.
    #[test]
    fn enter_is_one_undo_step_back_to_the_clip_before_the_session() {
        let notes = [(0, 240, 60), (960, 1200, 62)];
        let (mut seq, _) = session_seq(BAR * 4, BAR, &notes);
        let existing = note_on(BAR * 3);
        let existing_id = existing.id();
        {
            let clip = seq.selected_clip_mut().unwrap();
            clip.add_event(existing);
            clip.add_event(note_off(BAR * 3 + 240));
            clip.calculate_note_lengths();
            clip.select_event(Some(existing_id));
        }
        let before = lead_notes(&seq);

        assert!(seq.open_take_framing(ViewState::Clip));
        assert!(seq.set_take_edge_at(ClipEdge::End, BAR / 2));
        take_cursor_to(&mut seq, 960);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::Start));
        let mut record: Record<SequencerEdit> = Record::new();
        assert_eq!(inserted_take(enter(&mut seq, &mut record)), Some((62, 62)));
        let inserted = lead_notes(&seq);
        assert_eq!(
            inserted,
            vec![(BAR, BAR + 240, 62), (BAR * 3, BAR * 3 + 240, 60)]
        );
        assert_eq!(record.len(), 1);
        assert_eq!(
            seq.selected_clip().unwrap().selected_event_ids(),
            vec![existing_id]
        );

        assert_eq!(inserted_take(record.undo(&mut seq).unwrap()), None);
        assert_eq!(lead_notes(&seq), before);
        assert_eq!(
            inserted_take(record.redo(&mut seq).unwrap()),
            Some((62, 62))
        );
        assert_eq!(lead_notes(&seq), inserted);
    }

    /// Nothing to frame — an empty buffer, or the clip cursor at the clip's
    /// end — opens no session.
    #[test]
    fn nothing_to_frame_opens_no_session() {
        let (mut seq, _) = session_seq(BAR * 4, BAR, &[]);
        assert!(!seq.open_take_framing(ViewState::Clip));
        let (mut seq, _) = session_seq(BAR * 4, BAR * 4, &[(0, 240, 60)]);
        assert!(!seq.open_take_framing(ViewState::Clip));
        assert!(!seq.is_framing_take());
    }

    /// Framing while the loop plays: a window changed mid-pass is heard from
    /// the next loop wrap on, through the ordinary transport.
    #[test]
    fn a_window_changed_while_playing_is_heard_from_the_next_pass() {
        let notes = [(0, 240, 60), (960, 1200, 62)];
        let (mut seq, mut rx) = session_seq(BAR, 0, &notes);
        instrument_track_0(&mut seq);
        assert!(seq.open_take_framing(ViewState::Clip));

        seq.running.store(true, Ordering::Relaxed);
        seq.reset_to_tick(BAR * 4);
        assert_eq!(tick_and_collect(&mut seq, &mut rx, 480), vec![60]);

        take_cursor_to(&mut seq, 480);
        assert!(seq.set_take_edge_to_cursor(ClipEdge::End));
        tick_and_collect(&mut seq, &mut rx, BAR - 480);
        // The loop wraps: the transport re-seeks to the region start.
        seq.reset_to_tick(BAR * 4);
        assert_eq!(tick_and_collect(&mut seq, &mut rx, BAR), vec![60]);
    }

    /// Six notes a quarter apart (500–5780), their last off at 5780, with
    /// `wheels` (`(tick, msb)` pitch bends each) in the buffer, into a 4-bar
    /// lead clip holding `curve` with its clip cursor at `clip_cursor`;
    /// opened and entered. The lead clip's bends as `(tick, msb)`.
    fn bends_after_enter(
        clip_cursor: i32,
        curve: &[(i32, u8)],
        wheels: &[(i32, u8)],
    ) -> Vec<(i32, u8)> {
        let bend = |&(tick, msb): &(i32, u8)| bend_at(tick, msb);
        let notes: Vec<_> = (0..6).map(|i| (500 + i * 960, 980 + i * 960, 60)).collect();
        let (mut seq, _) = session_seq(BAR * 4, clip_cursor, &notes);
        seq.selected_clip_mut()
            .unwrap()
            .restore_events(curve.iter().map(bend).collect());
        for event in wheels.iter().map(bend) {
            seq.capture_clip.add_event(event);
        }
        seq.capture_clip.sort_events_by_tick();
        assert!(seq.open_take_framing(ViewState::Clip));
        enter(&mut seq, &mut Record::new());
        seq.lead_clip()
            .unwrap()
            .events()
            .iter()
            .filter(|event| !event.is_note_edge())
            .map(|event| (event.tick(), event.midi_message()[2]))
            .collect()
    }

    /// A bend easing back after the last note-off belongs to the phrase and
    /// lands as played; the clip's curve before the take's first move stays.
    #[test]
    fn an_ease_back_after_the_last_note_off_lands() {
        assert_eq!(
            bends_after_enter(BAR, &[(0, 0x50)], &[(5000, 0x60), (6000, 0x50)]),
            vec![(0, 0x50), (BAR + 4500, 0x60), (BAR + 5500, 0x50)]
        );
    }

    /// A bend still held when `/` was pressed drops to the clip's old value
    /// at the last note-off.
    #[test]
    fn a_held_bend_drops_to_the_old_value_at_the_last_note_off() {
        assert_eq!(
            bends_after_enter(BAR, &[(0, 0x50)], &[(5000, 0x60)]),
            vec![(0, 0x50), (BAR + 4500, 0x60), (BAR + 5280, 0x50)]
        );
    }

    /// Neither the moves nor the hand-back run past the clip's window end.
    #[test]
    fn the_wheels_stop_at_the_clips_window_end() {
        let near_end = BAR * 4 - 600;
        assert_eq!(
            bends_after_enter(near_end, &[], &[(600, 0x60), (6000, 0x40)]),
            vec![(near_end + 100, 0x60)]
        );
    }
}
