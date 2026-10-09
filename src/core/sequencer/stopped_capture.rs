//! The stopped `/`: framing the capture buffer by phrase detection, either as
//! a new clip ([`build_stopped_capture_clip`](Sequencer::build_stopped_capture_clip),
//! placed by `CommitClipEdit`) or as notes inserted into the lead clip
//! ([`build_stopped_capture_insert`](Sequencer::build_stopped_capture_insert),
//! by `InsertCaptureEdit`). Both are pure. Phrase-token detection — where the
//! last phrase starts — is in `region/`. See
//! `220-capture-without-pending-view.md` and `040-phrase-detection.md`.

use crate::{
    core::{
        config,
        time::{self, Meter},
    },
    models::{clip::Clip, event::EventType},
};

use super::{CaptureInsert, Sequencer, note_tick_bounds};

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

    /// The stopped `/` with a lead clip (`220-capture-without-pending-view.md`):
    /// the notes of the last phrase ([`detected_insert_window`](Self::detected_insert_window)),
    /// inserted into the selected clip at its clip cursor and cut off at its
    /// region end — a note running past it is closed there. The pure half of
    /// the insert; `InsertCaptureEdit` adds the notes and clears the buffer.
    /// `None` on an empty buffer, no lead clip, or no room at the cursor.
    pub(crate) fn build_stopped_capture_insert(&self) -> Option<CaptureInsert> {
        let track_idx = self.selected_track_index()?;
        let source_clip = self.selected_clip()?;
        let insert_at = source_clip.cursor_tick();
        let room = source_clip.region().end() - insert_at;
        if room <= 0 {
            return None;
        }

        let capture = self.detection_capture_source();
        let (start, end) = Self::detected_insert_window(&capture, self.meter())?;
        let length = (end - start).min(room);
        let mut phrase = Self::framed(capture, start, start + length);
        phrase.crop(length);

        let events = Self::capture_events_to_insert(&phrase, insert_at, length);
        (!events.is_empty()).then_some(CaptureInsert {
            track_idx,
            clip_id: source_clip.id(),
            events,
        })
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
    fn detection_capture_source(&self) -> Clip {
        let mut capture = Clip::new();
        capture.restore_events(self.capture_clip.events().to_vec());
        // Pairs the note lengths, over the trimmed buffer only.
        Self::trim_capture_source(&mut capture, self.meter());
        capture
    }

    /// The phrase a stopped insert takes from `capture` (as
    /// [`detection_capture_source`](Self::detection_capture_source) returns it):
    /// the window ending at the last `NoteOff`, as long as the played span but
    /// at most a bar, its start snapped to a phrase start. `None` on a capture
    /// without notes.
    fn detected_insert_window(capture: &Clip, meter: Meter) -> Option<(i32, i32)> {
        let (start, end) = note_tick_bounds(capture)?;
        let length = (end - start).min(meter.bar_ticks());
        Some(Self::snapped_window_from_last(
            capture,
            EventType::NoteOff,
            length,
            meter,
        ))
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
    fn framed(mut capture: Clip, start: i32, end: i32) -> Clip {
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

    use std::time::Instant;

    use rtrb::Consumer;
    use undo::Record;
    use uuid::Uuid;

    use super::*;
    use crate::{
        core::{
            config,
            sequencer::{
                ClipInstrumentEvent, CommitClipEdit, EditResult, InsertCaptureEdit, ResizeClipEdit,
                RetimeClipEdit, SequencerEdit,
                test_support::{clip_at, instrument_track_0, note_off, note_on, sequencer_with},
            },
        },
        models::event::Event,
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
        seq.set_global_region(0, time::bars_to_ticks(2));
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
        let bar = time::bars_to_ticks(1);
        let mut seq = make_seq();
        add_clip(&mut seq, bar * 40, bar);
        fill_capture_roughly(&mut seq, bar * 3 / 2);
        seq.cursor_tick.store(0, Ordering::Relaxed);
        let (_, plain) = seq.build_stopped_capture_clip().unwrap();

        let last = seq.capture_clip.events().last().unwrap().tick();
        let late = last + time::bars_to_ticks(config::CAPTURE_BUFFER_BARS) + bar;
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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
        let bar = time::bars_to_ticks(1);
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

    /// `ticks` sequencer ticks from wherever playback is, collecting the
    /// note numbers of the NoteOns that reached the instrument.
    fn tick_and_collect(
        seq: &mut Sequencer,
        rx: &mut Consumer<ClipInstrumentEvent>,
        ticks: i32,
    ) -> Vec<u8> {
        let at = Instant::now();
        let mut notes = Vec::new();
        for _ in 0..ticks {
            seq.tick(at);
            while let Ok(ev) = rx.pop() {
                if ev.message[0] & 0xf0 == 0x90 && ev.message[2] != 0 {
                    notes.push(ev.message[1]);
                }
            }
        }
        notes
    }

    /// Moving the start while playing (`220`, "Playback during edits"): the
    /// clip's events don't change, so the pass in progress plays out as it
    /// was — no jump to the new start at the moment of the edit — and the
    /// next loop wrap's seek picks up the new start.
    #[test]
    fn a_start_moved_while_playing_is_heard_from_the_next_wrap() {
        let bar = time::bars_to_ticks(1);
        let (mut seq, mut rx) = make_seq_with_rx();
        instrument_track_0(&mut seq);
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

        seq.reset_to_tick(0);
        assert_eq!(tick_and_collect(&mut seq, &mut rx, bar / 4), vec![60]);

        // `[` in the clip view at the second note: the window now starts there.
        let after = seq.selected_clip_start_marker_at(bar / 2).unwrap();
        let edit = ResizeClipEdit::for_clip(&seq, clip_id, after, None).unwrap();
        let mut record: Record<SequencerEdit> = Record::new();
        record.edit(&mut seq, SequencerEdit::ResizeClip(edit));

        let rest_of_pass = tick_and_collect(&mut seq, &mut rx, bar / 4 - 1);
        assert!(
            !rest_of_pass.contains(&62),
            "no jump to the new start mid-pass: {rest_of_pass:?}"
        );

        // The loop wraps (what the transport's wrap does to the sequencer).
        seq.reset_to_tick(0);
        assert_eq!(
            tick_and_collect(&mut seq, &mut rx, bar / 4),
            vec![62],
            "the new start from the next wrap"
        );
    }

    /// A four-bar lead clip at bar 5, its clip cursor at `clip_cursor` (event
    /// ticks), with `fill` in the capture buffer. Returns the clip's id.
    fn lead_clip_with_capture(
        seq: &mut Sequencer,
        clip_cursor: i32,
        fill: impl Fn(&mut Sequencer),
    ) -> Uuid {
        let bar = time::bars_to_ticks(1);
        add_clip(seq, bar * 4, bar * 4);
        let id = seq.selected_track().unwrap().clips()[0].id();
        seq.select_clip(Some(id));
        seq.selected_clip_mut()
            .unwrap()
            .set_cursor_tick(clip_cursor);
        fill(seq);
        id
    }

    /// Records the stopped `/` into the lead clip on `record`.
    fn insert_stopped_capture(seq: &mut Sequencer, record: &mut Record<SequencerEdit>) {
        let edit = InsertCaptureEdit::from_stopped_capture(seq).expect("a phrase to insert");
        record.edit(seq, SequencerEdit::InsertCapture(edit));
    }

    /// The lead clip's `(tick, is NoteOn)` events, sorted.
    fn lead_clip_notes(seq: &Sequencer) -> Vec<(i32, bool)> {
        let mut notes: Vec<_> = seq
            .selected_clip()
            .unwrap()
            .events()
            .iter()
            .map(|e| (e.tick(), e.event_type() == Some(EventType::NoteOn)))
            .collect();
        notes.sort_unstable();
        notes
    }

    /// Phase 4's must-not-regress: the stopped `/` into a lead clip inserts
    /// what an unedited `InsertIntoClip` pending phrase confirmed. These
    /// values were checked against that confirm before it was retired: the
    /// detected phrase starts on the clip cursor, an earlier phrase is left
    /// out, and a phrase running past the clip's end is cut there.
    #[test]
    fn stopped_insert_lands_the_detected_phrase_at_the_clip_cursor() {
        let bar = time::bars_to_ticks(1);
        let notes_after = |clip_cursor: i32, fill: &dyn Fn(&mut Sequencer)| {
            let mut seq = make_seq();
            lead_clip_with_capture(&mut seq, clip_cursor, fill);
            insert_stopped_capture(&mut seq, &mut Record::new());
            assert!(seq.capture_clip.events().is_empty(), "buffer consumed");
            lead_clip_notes(&seq)
        };

        // Six loose notes over a bar and a half: all of them, from the cursor.
        let loose = |seq: &mut Sequencer| fill_capture_roughly(seq, bar * 3 / 2);
        let expected: Vec<_> = (0..6)
            .flat_map(|i| {
                let on = bar + i * 960;
                let off = if i == 5 { on + 479 } else { on + 480 };
                [(on, true), (off, false)]
            })
            .collect();
        assert_eq!(notes_after(bar, &loose), expected);

        // Cursor 600 ticks before the clip end: only the first note fits.
        let near_end = bar * 4 - 600;
        assert_eq!(
            notes_after(near_end, &loose),
            vec![(near_end, true), (near_end + 480, false)]
        );

        // An early note, silence, then a four-note phrase: the phrase only.
        let two_phrases = |seq: &mut Sequencer| {
            seq.capture_clip.add_event(note_on(100));
            seq.capture_clip.add_event(note_off(400));
            for i in 0..4 {
                let t = bar * 3 + 300 + i * 960;
                seq.capture_clip.add_event(note_on(t));
                seq.capture_clip.add_event(note_off(t + 480));
            }
        };
        let expected: Vec<_> = (0..4)
            .flat_map(|i| {
                let on = 300 + i * 960;
                let off = if i == 3 { on + 479 } else { on + 480 };
                [(on, true), (off, false)]
            })
            .collect();
        assert_eq!(notes_after(300, &two_phrases), expected);
    }

    /// One ⌘Z takes the insert back and redo puts the same notes back, with
    /// the buffer left cleared; the event selection is never changed.
    #[test]
    fn stopped_insert_is_undoable_and_keeps_the_selection() {
        let bar = time::bars_to_ticks(1);
        let mut seq = make_seq();
        lead_clip_with_capture(&mut seq, bar, |seq| fill_capture_roughly(seq, bar));
        let existing = note_on(bar * 3);
        let existing_id = existing.id();
        {
            let clip = seq.selected_clip_mut().unwrap();
            clip.add_event(existing);
            clip.add_event(note_off(bar * 3 + 240));
            clip.calculate_note_lengths();
            clip.select_event(Some(existing_id));
        }
        let before = lead_clip_notes(&seq);

        let mut record: Record<SequencerEdit> = Record::new();
        insert_stopped_capture(&mut seq, &mut record);
        let inserted = lead_clip_notes(&seq);
        assert!(inserted.len() > before.len());
        assert_eq!(
            seq.selected_clip().unwrap().selected_event_ids(),
            vec![existing_id]
        );

        record.undo(&mut seq);
        assert_eq!(lead_clip_notes(&seq), before);
        assert!(
            seq.capture_clip.events().is_empty(),
            "undo leaves it cleared"
        );
        assert_eq!(
            seq.selected_clip().unwrap().selected_event_ids(),
            vec![existing_id]
        );

        record.redo(&mut seq);
        assert_eq!(lead_clip_notes(&seq), inserted);
    }

    /// The insert and its redo report the take's pitch range, so the piano
    /// roll re-frames over it either way (`UiEvent::CaptureInserted`); the
    /// undo reports none.
    #[test]
    fn stopped_insert_reports_its_take_on_edit_and_redo_only() {
        let bar = time::bars_to_ticks(1);
        let mut seq = make_seq();
        lead_clip_with_capture(&mut seq, bar, |seq| fill_capture_roughly(seq, bar));
        let take = |result: EditResult| match result {
            EditResult::EventsModified { inserted_take, .. } => inserted_take,
            _ => panic!("an events edit"),
        };

        let mut record: Record<SequencerEdit> = Record::new();
        let edit = InsertCaptureEdit::from_stopped_capture(&seq).expect("a phrase to insert");
        // Every captured note is middle C.
        assert_eq!(
            take(record.edit(&mut seq, SequencerEdit::InsertCapture(edit))),
            Some((60, 60))
        );
        assert_eq!(take(record.undo(&mut seq).unwrap()), None);
        assert_eq!(take(record.redo(&mut seq).unwrap()), Some((60, 60)));
    }

    /// Nothing to insert — an empty buffer, or a cursor at the clip's end —
    /// builds no edit, so nothing enters the undo record.
    #[test]
    fn stopped_insert_with_nothing_to_insert_builds_no_edit() {
        let bar = time::bars_to_ticks(1);
        let mut seq = make_seq();
        lead_clip_with_capture(&mut seq, bar, |_| {});
        assert!(InsertCaptureEdit::from_stopped_capture(&seq).is_none());

        let mut seq = make_seq();
        lead_clip_with_capture(&mut seq, bar * 4, |seq| fill_capture_roughly(seq, bar));
        assert!(InsertCaptureEdit::from_stopped_capture(&seq).is_none());
    }
}
