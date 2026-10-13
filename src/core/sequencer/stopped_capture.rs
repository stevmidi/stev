//! The stopped `/`: framing the capture buffer by phrase detection as a new
//! clip ([`build_stopped_capture_clip`](Sequencer::build_stopped_capture_clip),
//! pure, placed by `CommitClipEdit`). With a lead clip the stopped `/` opens a
//! take session instead (`take_framing.rs`), which frames the same capture
//! source. Phrase-token detection — where the last phrase starts — is in
//! `region/`. See `220-capture-without-pending-view.md`,
//! `290-framing-a-take.md` and `040-phrase-detection.md`.

use crate::{
    core::{
        config,
        time::{self, Meter},
    },
    models::clip::Clip,
};

use super::Sequencer;

#[cfg(debug_assertions)]
use super::capture_fixture::CaptureFixture;

impl Sequencer {
    /// The stopped `/` with no lead clip (`220-capture-without-pending-view.md`):
    /// a new clip framed by phrase detection and placed at the cursor on the
    /// selected track, with swing detection. A later clip is rounded to the
    /// nearest whole bars; the project's first clip keeps its exact window
    /// and leaves the tempo alone — Enter fits the tempo to it
    /// (`RetimeClipEdit`). It is **not cropped**: the capture source (the last `CAPTURE_BUFFER_BARS`) stays in
    /// the clip as material outside its window, rebased so the window starts
    /// on a bar line in event space. Pure. `None` on a buffer without a note,
    /// no selected track, or no room at the cursor.
    pub(crate) fn build_stopped_capture_clip(&self) -> Option<(usize, Clip)> {
        if !self.capture_clip.has_notes() {
            return None;
        }
        let track_idx = self.selected_track_index()?;
        let mut clip = self.build_detected_phrase_clip();

        // The project's first clip keeps its exact detected window: its
        // tempo changes only when Enter fits it (`RetimeClipEdit`) — the
        // length it ends up with *is* the tempo, so rounding it here would
        // throw away what the user sets by ear. Every later clip is nearest
        // whole bars, but never past the next clip: floored to the bars that fit
        // there, or the bare gap when not even one bar does.
        let bar = self.meter().bar_ticks();
        let raw = clip.region_length();
        let length = if self.number_of_clips() == 0 {
            raw
        } else {
            time::snap_to_grid(raw, bar).max(bar)
        };
        let length = self.fit_length_before_next_clip(clip.start_tick(), length);
        let region_start = clip.region().start();
        clip.region_mut()
            .set_region(None, Some(region_start + length));

        // Put the window start on a bar line in event space, so the clip's
        // grid (quantize, swing, the clip view) is the phrase's.
        clip.align_window_start_to_bar(self.meter());
        clip.nudge_cursor_to_region_start();

        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip.detect_and_store_swing();

        self.tracks
            .get(track_idx)?
            .fits(&clip)
            .then_some((track_idx, clip))
    }

    /// Debug builds: dumps the capture a stopped `/` is about to frame as a
    /// [`CaptureFixture`], when `STEV_CAPTURE_DIR` asks for it
    /// (`capture_fixture.rs`). Call before the commit consumes the buffer.
    #[cfg(debug_assertions)]
    pub(crate) fn write_stopped_capture_fixture(&self) {
        let capture = self.detection_capture_source();
        if !capture.has_notes() {
            return;
        }

        let loop_reference_length = self.loop_reference_length();
        let meter = self.meter();
        let (detected_start, _) =
            Self::detected_phrase_window(&capture, loop_reference_length, meter);
        CaptureFixture::from_capture(
            &capture,
            self.tempo_us(),
            meter,
            loop_reference_length,
            detected_start,
        )
        .write_if_requested();
    }

    /// The capture buffer as phrase detection reads it: sorted, note lengths
    /// calculated, trimmed to the last
    /// [`CAPTURE_BUFFER_BARS`](config::CAPTURE_BUFFER_BARS). A fresh clip —
    /// its own region and cursor, never the capture buffer's shared ones —
    /// so the builders can frame it in place.
    pub(super) fn detection_capture_source(&self) -> Clip {
        let mut capture = Clip::new();
        capture.restore_events(self.capture_clip.events().to_vec());
        // Pairs the note lengths, over the trimmed buffer only.
        Self::trim_capture_source(&mut capture, self.meter());
        capture
    }

    /// The detected phrase as a clip: every event of the capture source in
    /// absolute tick space, the window from the detected phrase start to the
    /// last `NoteOff` plus two beats (at least a bar), placed at the cursor.
    fn build_detected_phrase_clip(&self) -> Clip {
        let capture = self.detection_capture_source();
        let meter = self.meter();
        let (region_start_tick, end) =
            Self::detected_phrase_window(&capture, self.loop_reference_length(), meter);
        let maybe_default_tail_end =
            Self::phrase_end_from_last_note_off_with_tail(&capture, region_start_tick);
        let region_end_tick = Self::clamp_phrase_end_to_min_window(
            region_start_tick,
            maybe_default_tail_end.unwrap_or(end),
            meter,
        );

        let mut clip = Self::framed(capture, region_start_tick, region_end_tick);
        clip.set_start_tick(self.cursor_tick());
        clip
    }

    /// `capture` (a [`detection_capture_source`](Self::detection_capture_source))
    /// with its window set to `[start, end)`.
    pub(super) fn framed(mut capture: Clip, start: i32, end: i32) -> Clip {
        capture.region_mut().set_region(Some(start), Some(end));
        capture
    }

    /// Drops all but the most recent [`CAPTURE_BUFFER_BARS`](config::CAPTURE_BUFFER_BARS)
    /// of the capture source (sorted), so a long recording session can't
    /// influence phrase-token detection, and calculates the survivors' note
    /// lengths. The buffer ends at its last note edge's tick — no note ends
    /// later than its own note-off, so the cutoff needs no lengths paired
    /// first; a wheel moved after the last note (a bend easing back) doesn't
    /// move it.
    fn trim_capture_source(capture: &mut Clip, meter: Meter) {
        let Some(capture_end_tick) = capture
            .events()
            .iter()
            .rev()
            .find(|event| event.is_note_edge())
            .map(|event| event.tick())
        else {
            return;
        };

        let buffer_ticks = meter.bars_to_ticks(config::CAPTURE_BUFFER_BARS);
        let cutoff_tick = (capture_end_tick - buffer_ticks).max(0);
        capture.trim_before_tick(cutoff_tick);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use rtrb::Consumer;
    use undo::Record;
    use uuid::Uuid;

    use super::*;
    use crate::{
        core::{
            config,
            sequencer::{
                ClipInstrumentEvent, CommitClipEdit, ResizeClipEdit, RetimeClipEdit, SequencerEdit,
                test_support::{
                    clip_at, instrument_track_0, note_off, note_on, sequencer_with,
                    tick_and_collect, tick_and_collect_messages,
                },
            },
        },
        models::{
            clip::ClipEdge,
            event::{Event, EventType},
        },
    };

    /// Minimal sequencer: track 0 selected, region `[0, 2 bars]`, default tempo.
    fn make_seq() -> Sequencer {
        make_seq_with_rx().0
    }

    fn add_clip(seq: &mut Sequencer, start_tick: i32, region_len: i32) {
        seq.selected_track_mut()
            .unwrap()
            .add_clip(&clip_at(start_tick, region_len));
    }

    /// Records `bars`-and-a-bit worth of loose material into the capture buffer.
    fn fill_capture_roughly(seq: &mut Sequencer, span_ticks: i32) {
        let step = span_ticks / 6;
        for i in 0..6 {
            let t = 500 + i * step;
            seq.capture_clip.add_event(note_on(t));
            seq.capture_clip.add_event(note_off(t + step / 2));
        }
    }

    /// Plays a clip across its whole span (a little into the next loop) and
    /// returns every NoteOn number that reached the instrument bus.
    fn play_and_collect_note_ons(
        seq: &mut Sequencer,
        rx: &mut Consumer<ClipInstrumentEvent>,
        clip_start: i32,
        clip_len: i32,
    ) -> Vec<u8> {
        seq.reset_to_tick(clip_start);
        tick_and_collect(seq, rx, clip_len + 64)
    }

    fn make_seq_with_rx() -> (Sequencer, Consumer<ClipInstrumentEvent>) {
        let (mut seq, plugin_rx) = sequencer_with(true);
        seq.set_global_region(0, Meter::FOUR_FOUR.bars_to_ticks(2));
        let track_id = seq.track_id_by_index(0).unwrap();
        seq.select_track(Some(track_id));
        (seq, plugin_rx)
    }

    fn bpm(seq: &Sequencer) -> f32 {
        time::tempo_us_to_bpm(seq.tempo_us())
    }

    // --- Stopped `/` commit (220-capture-without-pending-view.md) ---

    /// NoteOn ticks inside a clip's window, relative to the window start.
    fn window_note_ons(clip: &Clip) -> Vec<i32> {
        let start = clip.region().start();
        clip.events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| e.tick())
            .filter(|&t| clip.is_in_window(t))
            .map(|t| t - start)
            .collect()
    }

    /// Records the stopped `/` on `record` and selects the clip it placed at
    /// the cursor.
    fn commit_stopped(seq: &mut Sequencer, record: &mut Record<SequencerEdit>) {
        let edit = CommitClipEdit::from_stopped_capture(seq).expect("a clip to commit");
        record.edit(seq, SequencerEdit::CommitClip(edit));
        let cursor = seq.cursor_tick();
        let id = seq
            .selected_track()
            .unwrap()
            .clips()
            .iter()
            .find(|c| c.start_tick() == cursor)
            .expect("the committed clip at the cursor")
            .id();
        seq.select_clip(Some(id));
    }

    /// Records Enter's tempo fit to the project's only clip on `record`.
    fn fit_tempo(seq: &mut Sequencer, record: &mut Record<SequencerEdit>) {
        let fit = RetimeClipEdit::fit_sole_clip(seq).expect("a clip to fit");
        record.edit(seq, SequencerEdit::RetimeClip(fit));
    }

    /// A bend easing back after the last note-off — however long after —
    /// neither stretches the take nor trims its notes out of the capture
    /// buffer (which is measured back from the last note edge).
    #[test]
    fn a_wheel_moved_after_the_last_note_does_not_move_the_take() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        add_clip(&mut seq, bar * 40, bar);
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);
        let (_, plain) = seq.build_stopped_capture_clip().unwrap();

        let last = seq.capture_clip.events().last().unwrap().tick();
        let late = last + Meter::FOUR_FOUR.bars_to_ticks(config::CAPTURE_BUFFER_BARS) + bar;
        seq.capture_clip
            .add_event(Event::new(late, 0, vec![0xE0, 0x00, 0x40]));
        let (_, with_bend) = seq.build_stopped_capture_clip().unwrap();

        assert_eq!(with_bend.region_length(), plain.region_length());
        assert_eq!(window_note_ons(&with_bend), window_note_ons(&plain));
    }

    /// The stopped `/`'s window for a later clip, pinned to what an unedited
    /// pending-phrase confirm produced before the pending view was retired
    /// (checked equal while both existed): two whole bars from the first
    /// note, the tempo untouched.
    #[test]
    fn stopped_commit_window_is_the_retired_confirms() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        add_clip(&mut seq, bar * 20, bar);
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let (_, clip) = seq.build_stopped_capture_clip().unwrap();

        assert_eq!(clip.start_tick(), 0);
        assert_eq!(clip.region_length(), bar * 2);
        assert_eq!(window_note_ons(&clip), vec![0, 960, 1920, 2880, 3840, 4800]);
        assert_eq!(seq.tempo_us(), config::TEMPO_US_DEFAULT);
    }

    /// In 3/4 a later clip is whole 3/4 bars with its window on a 3/4 bar
    /// line, and Enter fits the first clip to whole 3/4 bars.
    #[test]
    fn stopped_commit_and_enter_count_bars_in_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let bar = three_four.bar_ticks();

        let mut seq = make_seq();
        seq.set_meter(three_four);
        add_clip(&mut seq, bar * 20, bar);
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);
        let (_, clip) = seq.build_stopped_capture_clip().unwrap();
        assert_eq!(clip.region_length(), bar * 2);
        assert_eq!(clip.region().start() % bar, 0);

        let mut seq = make_seq();
        seq.set_meter(three_four);
        seq.region_end.store(bar * 8, Ordering::Relaxed);
        fill_capture_roughly(&mut seq, bar * 5 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);
        let mut record: Record<SequencerEdit> = Record::new();
        commit_stopped(&mut seq, &mut record);
        fit_tempo(&mut seq, &mut record);
        let fitted = seq.selected_clip().unwrap();
        assert_eq!(fitted.region_length() % bar, 0, "whole 3/4 bars");
        assert_eq!(fitted.region().start() % bar, 0, "on a 3/4 bar line");
    }

    /// The project's first clip keeps the exact detected window and leaves
    /// the tempo alone; Enter's fit then lands on the tempo, length and notes
    /// the retired confirm produced (pinned, checked equal while both
    /// existed).
    #[test]
    fn stopped_first_clip_is_exact_and_enter_fits_it() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        seq.region_end.store(bar * 8, Ordering::Relaxed);
        fill_capture_roughly(&mut seq, bar * 5 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let mut record: Record<SequencerEdit> = Record::new();
        commit_stopped(&mut seq, &mut record);
        assert_eq!(seq.tempo_us(), config::TEMPO_US_DEFAULT, "tempo untouched");
        assert_ne!(
            seq.selected_clip().unwrap().region_length() % bar,
            0,
            "the exact window, not rounded"
        );

        fit_tempo(&mut seq, &mut record);
        let fitted = seq.selected_clip().unwrap();

        assert_eq!(seq.tempo_us(), 620_370);
        assert_eq!(fitted.region_length(), bar * 3);
        assert_eq!(
            window_note_ons(fitted),
            vec![0, 1719, 3438, 5157, 6876, 8595]
        );
    }

    /// Enter fits the first clip to the nearest whole bar of what was
    /// *played* — not to the loop region, which here is far larger.
    #[test]
    fn enter_fits_the_first_clip_to_played_bars_not_the_loop() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        seq.region_end.store(bar * 8, Ordering::Relaxed); // 8-bar loop
        let start_tempo = seq.tempo_us();
        fill_capture_roughly(&mut seq, bar * 5 / 2); // ~2.5 bars of material
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let mut record: Record<SequencerEdit> = Record::new();
        commit_stopped(&mut seq, &mut record);
        fit_tempo(&mut seq, &mut record);

        let clip = seq.selected_clip().unwrap();
        assert_eq!(clip.region_length() % bar, 0, "clip must be whole bars");
        assert!(
            clip.region_length() <= bar * 4,
            "phrase must not stretch to fill the 8-bar loop, got {} ticks",
            clip.region_length()
        );
        assert_ne!(seq.tempo_us(), start_tempo, "tempo should have been fitted");
    }

    /// End-to-end: fitting the first phrase against a fast project tempo
    /// still lands the project tempo in a plausible band.
    #[test]
    fn enter_keeps_a_first_clip_tempo_in_band_from_a_fast_project() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        seq.set_tempo(time::bpm_to_tempo_us(175)); // 175 BPM project
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let mut record: Record<SequencerEdit> = Record::new();
        commit_stopped(&mut seq, &mut record);
        fit_tempo(&mut seq, &mut record);

        let b = bpm(&seq);
        assert!(
            b > 50.0 && b < 140.0,
            "fitted first clip tempo out of band: {b}"
        );
    }

    /// A single ~1-bar phrase recorded a few passes into a big loop: the
    /// detected window holds its notes.
    #[test]
    fn stopped_commit_window_covers_the_phrase_with_a_large_loop() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        seq.region_start.store(0, Ordering::Relaxed);
        seq.region_end.store(bar * 16, Ordering::Relaxed);
        let base = bar * 40;
        for beat in 0..4 {
            let t = base + beat * (bar / 4);
            seq.capture_clip.add_event(note_on(t));
            seq.capture_clip.add_event(note_off(t + bar / 8));
        }
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let (_, clip) = seq.build_stopped_capture_clip().unwrap();

        assert_eq!(
            window_note_ons(&clip).len(),
            4,
            "every phrase note in the window"
        );
    }

    /// Regression: recording live against a loop *larger* than the phrase,
    /// with earlier loop passes still in the capture buffer — the committed
    /// clip must play its notes into the instrument bus.
    #[test]
    fn stopped_commit_from_a_long_session_plays_its_notes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut seq, mut rx) = make_seq_with_rx();
        instrument_track_0(&mut seq);

        // 4-bar loop, capture stamped several passes in (high absolute ticks),
        // holding ~3 passes of a 1-bar phrase — like a real looping session.
        seq.region_start.store(bar, Ordering::Relaxed);
        seq.region_end.store(bar * 5, Ordering::Relaxed);
        for pass in 0..3 {
            let base = bar * 4 * 6 + pass * bar; // deep into the take
            for beat in 0..4 {
                let t = base + beat * (bar / 4);
                seq.capture_clip.add_event(note_on(t));
                seq.capture_clip.add_event(note_off(t + bar / 8));
            }
        }
        seq.cursor_tick.store(bar, Ordering::Relaxed);

        commit_stopped(&mut seq, &mut Record::new());
        let (start, len) = {
            let clip = seq.selected_clip().unwrap();
            (clip.start_tick(), clip.region_length())
        };

        let notes = play_and_collect_note_ons(&mut seq, &mut rx, start, len);
        assert!(
            !notes.is_empty(),
            "the committed clip must play; got silence"
        );
    }

    /// The whole capture source stays in the clip, the window starts on a bar
    /// line in event space, and the cursor sits on it.
    #[test]
    fn stopped_commit_keeps_the_capture_outside_the_window() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        add_clip(&mut seq, bar * 20, bar);
        // An early phrase, a two-bar silence, then the phrase detection keeps.
        seq.capture_clip.add_event(note_on(100));
        seq.capture_clip.add_event(note_off(400));
        for i in 0..4 {
            let t = bar * 3 + 300 + i * 960;
            seq.capture_clip.add_event(note_on(t));
            seq.capture_clip.add_event(note_off(t + 480));
        }
        let captured = seq.capture_clip.events().len();
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let (_, clip) = seq.build_stopped_capture_clip().unwrap();

        assert_eq!(clip.events().len(), captured, "nothing is cropped away");
        assert_eq!(
            clip.region().start() % bar,
            0,
            "window starts on a bar line"
        );
        assert_eq!(clip.region_length() % bar, 0, "whole bars");
        assert_eq!(clip.cursor_tick(), clip.region().start());
        assert!(
            clip.events()
                .iter()
                .any(|e| e.tick() < clip.region().start()),
            "the early phrase is kept before the window"
        );
    }

    /// Rounding to whole bars never runs into the next clip: it floors to the
    /// bars that fit instead.
    #[test]
    fn stopped_commit_floors_to_the_bars_before_the_next_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        add_clip(&mut seq, bar, bar); // one bar of room at the cursor
        fill_capture_roughly(&mut seq, bar * 5 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let (_, clip) = seq.build_stopped_capture_clip().unwrap();

        assert_eq!(clip.region_length(), bar);
        assert!(seq.selected_track().unwrap().fits(&clip));
    }

    /// Recorded as a `CommitClipEdit`: the first edit places the clip and
    /// clears the buffer, undo lifts it, redo restores the same id and window.
    #[test]
    fn stopped_commit_is_undoable() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut seq = make_seq();
        add_clip(&mut seq, bar * 20, bar);
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let mut record: Record<SequencerEdit> = Record::new();
        let edit = CommitClipEdit::from_stopped_capture(&seq).unwrap();
        record.edit(&mut seq, SequencerEdit::CommitClip(edit));

        let (id, len) = {
            let clip = seq.selected_track().unwrap().clips().first().unwrap();
            assert_eq!(clip.start_tick(), 0);
            (clip.id(), clip.region_length())
        };
        assert!(seq.capture_clip.events().is_empty(), "buffer consumed");

        record.undo(&mut seq);
        assert!(seq.selected_track().unwrap().get_clip_by_id(id).is_none());

        record.redo(&mut seq);
        let restored = seq.selected_track().unwrap().get_clip_by_id(id).unwrap();
        assert_eq!(restored.region_length(), len);
    }

    /// The notes kept outside the window stay silent: playing the committed
    /// clip sounds only the phrase detection framed.
    #[test]
    fn stopped_commit_plays_only_its_window() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut seq, mut rx) = make_seq_with_rx();
        instrument_track_0(&mut seq);
        // An early note 50, two bars of silence, then a phrase of note 60.
        seq.capture_clip
            .add_event(Event::new(100, 0, vec![0x90, 50, 100]));
        seq.capture_clip
            .add_event(Event::new(400, 0, vec![0x80, 50, 0]));
        for i in 0..4 {
            let t = bar * 3 + 300 + i * 960;
            seq.capture_clip.add_event(note_on(t));
            seq.capture_clip.add_event(note_off(t + 480));
        }
        seq.cursor_tick.store(0, Ordering::Relaxed);

        let edit = CommitClipEdit::from_stopped_capture(&seq).unwrap();
        let mut record: Record<SequencerEdit> = Record::new();
        record.edit(&mut seq, SequencerEdit::CommitClip(edit));
        let (start, len) = {
            let clip = seq.selected_track().unwrap().clips().first().unwrap();
            assert!(clip.events().iter().any(|e| e.note_number() == Some(50)));
            (clip.start_tick(), clip.region_length())
        };

        let notes = play_and_collect_note_ons(&mut seq, &mut rx, start, len);
        assert!(notes.contains(&60), "the phrase plays; got {notes:?}");
        assert!(
            !notes.contains(&50),
            "the kept early note must not; got {notes:?}"
        );
    }

    /// A playing one-bar clip at bar 1 holding note 60 over the first eighth
    /// and note 62 over the eighth from the half bar, the playhead walked
    /// `ticks` into it, the clip selected. Returns the clip's id.
    fn playing_two_note_clip(
        seq: &mut Sequencer,
        rx: &mut Consumer<ClipInstrumentEvent>,
        ticks: i32,
    ) -> Uuid {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        instrument_track_0(seq);
        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(bar));
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        clip.add_event(Event::new(bar / 8, 0, vec![0x80, 60, 0]));
        clip.add_event(Event::new(bar / 2, 0, vec![0x90, 62, 100]));
        clip.add_event(Event::new(bar / 2 + bar / 8, 0, vec![0x80, 62, 0]));
        clip.calculate_note_lengths();
        let clip_id = clip.id();
        seq.tracks_mut()[0].add_clip(&clip);
        seq.select_clip(Some(clip_id));
        seq.running.store(true, Ordering::Relaxed);
        seq.reset_to_tick(0);
        tick_and_collect_messages(seq, rx, ticks);
        clip_id
    }

    /// `[` in the clip view at `event_tick`, recorded as the edit does it.
    fn move_start(seq: &mut Sequencer, clip_id: Uuid, event_tick: i32) {
        let after = seq
            .selected_clip_marker_at(ClipEdge::Start, event_tick)
            .unwrap()
            .bounds;
        let edit = ResizeClipEdit::for_clip(seq, clip_id, after, None).unwrap();
        let mut record: Record<SequencerEdit> = Record::new();
        record.edit(seq, SequencerEdit::ResizeClip(edit));
    }

    /// Moving the start while playing (`220`, "Playback during edits") is
    /// heard at once: the playhead stays where it is and the clip plays its
    /// new window from the matching point — not the old window walked on
    /// until the next wrap, which played the rest of the pass out of phase
    /// (found 2026-10-12).
    #[test]
    fn a_start_moved_while_playing_is_heard_at_once() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut seq, mut rx) = make_seq_with_rx();
        let clip_id = playing_two_note_clip(&mut seq, &mut rx, bar / 4);

        // The window now starts a quarter bar later, so the playhead's
        // quarter bar into the clip is the half bar of the take: note 62.
        move_start(&mut seq, clip_id, bar / 4);
        assert_eq!(
            tick_and_collect_messages(&mut seq, &mut rx, 8),
            vec![(0x90, 62)],
            "the new window from the playhead"
        );
        // ...and it ends where the new window does, an eighth later.
        assert_eq!(
            tick_and_collect_messages(&mut seq, &mut rx, bar / 8),
            vec![(0x80, 62)]
        );
    }

    /// A note sounding when the start moves is released if the new point
    /// has it silent, and a note sounding at the new point is chased in.
    #[test]
    fn a_start_moved_while_playing_releases_and_chases_notes() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut seq, mut rx) = make_seq_with_rx();
        // A sixteenth in: note 60 is sounding.
        let clip_id = playing_two_note_clip(&mut seq, &mut rx, bar / 16);

        // The window now starts on note 62, so the playhead's sixteenth into
        // the clip is a sixteenth into that note.
        move_start(&mut seq, clip_id, bar / 2);
        let messages = tick_and_collect_messages(&mut seq, &mut rx, 4);
        assert!(messages.contains(&(0x80, 60)), "60 released: {messages:?}");
        assert!(messages.contains(&(0x90, 62)), "62 chased in: {messages:?}");

        // 62 ends where the walk meets its own note-off, not twice.
        let rest = tick_and_collect_messages(&mut seq, &mut rx, bar / 8);
        assert_eq!(rest, vec![(0x80, 62)]);
    }
}
