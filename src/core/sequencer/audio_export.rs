//! The audio export (`⌘/Ctrl+⇧+R`, `130-plugin-host.md` § Audio export), its sequencer
//! half: the plan (which tracks get a stem, what each file is called, the
//! stretch of time it covers) and the run's state while it renders. The
//! workflow that starts and ends the run is the handler's; the tap and the
//! files are the plugin host's.
//!
//! The run is rendered offline, in *virtual time* (`490-offline-export.md`
//! decisions 2 and 4): the `"audio-engine"` thread renders with the device
//! closed and, before each block, asks for the ticks due before it
//! ([`ExportStep`]). Tick `n` of the run carries the instant `start + n ×
//! tick length`, worked out from the tempo in integers, whenever it is
//! stepped; nothing on the way to the mixer compares it with now. The run
//! sends the [`ExportMark`]s from the tick path, stamped with the instants
//! of its first tick and of the one a selection's length later, so the
//! mixer cuts every file where those ticks sound and the files are the
//! selection to the sample.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::core::{
    config::MAX_TRACKS,
    input_event::TimeSelectionRect,
    project::stem_file_name,
    time::{ticks_to_millis_f64, ticks_to_nanos},
};

use super::Sequencer;

/// A set of engine slots (`Track::slot`), bit `n` for slot `n`.
pub(crate) type SlotMask = u32;

// Every slot has a bit.
const _: () = assert!(MAX_TRACKS <= SlotMask::BITS as usize);

/// One end of the export, from the sequencer to the plugin host's mixer on
/// its own `rtrb` ring. The mixer turns `at` into a frame the way it places
/// a note-on (`RenderCtx::scheduled_frame`), so the files start and end on
/// the samples where those ticks sound.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum ExportMark {
    /// The selection's start tick: sample 0 of every file.
    Start {
        /// The export's id ([`AudioExportJob::id`]), which the mixer reports
        /// the end by.
        id: u32,
        /// The tick's intended time.
        at: Instant,
        /// The engine slots that get a stem.
        slots: SlotMask,
    },
    /// The selection's end tick: the first sample past every file.
    End {
        /// The tick's intended time.
        at: Instant,
    },
}

/// The offline render asking the `"sequencer"` thread for the ticks due
/// before `until`, a block's first instant, and waiting for the answer
/// before it renders the block (`490-offline-export.md` decision 3).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ExportStep {
    /// Every tick of the run due before this is to be played.
    pub(crate) until: Instant,
    /// Where the answer goes: `true` once the ticks due have been played
    /// and the block can be rendered, `false` when no export is running and
    /// the render should stop.
    pub(crate) answer: Sender<bool>,
}

/// The files one export writes, sent to the `"audio-export"` thread, which
/// opens them and has the engine render the run. The folder already exists.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct AudioExportJob {
    /// The export's id, as its start mark carries it.
    pub(crate) id: u32,
    /// The instant of the run's first tick ([`AudioExportRun::start`]),
    /// which the render anchors its first frame at.
    pub(crate) start: Instant,
    /// Each stem's engine slot and file.
    pub(crate) stems: Vec<(usize, PathBuf)>,
    /// The mix file.
    pub(crate) mix: PathBuf,
}

/// How the `"audio-export"` thread's files came out, sent back once it is
/// done with them (`SequencerCommand::AudioExportWritten`).
#[derive(Debug, Default, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct AudioExportOutcome {
    /// Why the writer stopped before the mixer ended the export, if it did:
    /// the render stopped coming, so the files are cut short.
    pub(crate) stopped: Option<&'static str>,
    /// The first file error; what was written before it stays on disk.
    pub(crate) error: Option<String>,
    /// The stem slots with no plugin for some or all of the run: their files
    /// are silence there.
    pub(crate) silent: Vec<usize>,
}

/// An export in progress: its plan, where it writes, and what to put back
/// when it ends.
pub(crate) struct AudioExportRun {
    /// What's exported.
    pub(crate) plan: AudioExportPlan,
    /// The export's folder.
    pub(crate) folder: PathBuf,
    /// Whether looping was on before the run turned it off.
    pub(crate) loop_was_enabled: bool,
    /// Esc cancelled it: its files go once the writer is done.
    pub(crate) cancelled: bool,
    /// The instant of the run's first tick. Made up: the run's time moves
    /// only as it is stepped.
    start: Instant,
    /// The tempo the run began at, µs per quarter note: the tick length. It
    /// can't change mid-run (the run blocks the command).
    tempo_us: i32,
    /// How many ticks of the run have been played.
    ticks: u64,
    /// Whether its end mark has gone to the mixer, which will end the
    /// export. The start mark went with tick 0, or goes with a close before
    /// it: `ticks` says which.
    ended: bool,
}

impl AudioExportRun {
    /// The instant tick `n` of the run carries: `start + n × tick length`,
    /// from the tempo in integers ([`ticks_to_nanos`]), so it never drifts.
    fn tick_instant(&self, n: u64) -> Instant {
        self.start + Duration::from_nanos(ticks_to_nanos(self.tempo_us, n))
    }

    /// The run's time now: the instant of the next tick to play.
    fn now(&self) -> Instant {
        self.tick_instant(self.ticks)
    }
}

/// What an export will write: one stem per track in [`stems`](Self::stems)
/// plus the mix, every file covering `[start_tick, end_tick)`.
#[derive(Debug, PartialEq)]
pub(crate) struct AudioExportPlan {
    /// First tick of the export: the time selection's start, sample 0 of
    /// every file.
    pub(crate) start_tick: i32,
    /// Where the selection ends (exclusive).
    pub(crate) end_tick: i32,
    /// The plugin tracks that get a stem, in arrangement order.
    pub(crate) stems: Vec<StemPlan>,
    /// Positions of the MIDI Out tracks that would have been heard but whose
    /// audio never reaches the app, so the footer can say they were left out.
    pub(crate) left_out: Vec<usize>,
}

/// One track's stem.
#[derive(Debug, PartialEq)]
pub(crate) struct StemPlan {
    /// The track's engine slot (`Track::slot`): the voice the mixer taps.
    pub(crate) slot: usize,
    /// The stem's file name ([`stem_file_name`]).
    pub(crate) file_name: String,
}

/// Why nothing would be exported. Never silent: the handler says so in the
/// footer.
#[derive(Debug, PartialEq)]
pub(crate) enum AudioExportRefusal {
    /// No arranger time selection, or one with no tick range: the selection
    /// is what sets the export's length.
    NoSelection,
    /// No audible plugin track has an unmuted clip in the selection.
    NothingToHear,
}

impl Sequencer {
    /// The plan for exporting `selection` (the arranger's time selection,
    /// `None` from the clip view). The selection chooses only the time: every
    /// track audible under mute and solo with an unmuted clip overlapping it
    /// counts, on whichever tracks the selection was drawn. Of those, a
    /// plugin track gets a stem and a MIDI Out track is listed as left out.
    pub(crate) fn audio_export_plan(
        &self,
        selection: Option<TimeSelectionRect>,
    ) -> Result<AudioExportPlan, AudioExportRefusal> {
        let Some(selection) = selection.filter(TimeSelectionRect::has_tick_range) else {
            return Err(AudioExportRefusal::NoSelection);
        };
        let (start_tick, end_tick) = (selection.start, selection.end);

        let audible = self.track_audibility();
        let mut stems = Vec::new();
        let mut left_out = Vec::new();
        for (position, track) in self.tracks().iter().enumerate() {
            let plays_here = track
                .clips()
                .iter()
                .any(|c| !c.is_muted() && c.start_tick() < end_tick && c.end_tick() > start_tick);
            if !audible[position] || !plays_here {
                continue;
            }
            match track.output().instrument() {
                Some(instrument) => stems.push(StemPlan {
                    slot: track.slot(),
                    // The track's own name, or for a track never named its
                    // plugin's: the header's bare number would say nothing
                    // in a DAW that the file's `NN` doesn't already.
                    file_name: stem_file_name(
                        position + 1,
                        track.name().unwrap_or(&instrument.display_name),
                    ),
                }),
                None => left_out.push(position),
            }
        }

        if stems.is_empty() {
            return Err(AudioExportRefusal::NothingToHear);
        }
        Ok(AudioExportPlan {
            start_tick,
            end_tick,
            stems,
            left_out,
        })
    }

    /// Starts the run for `plan`, writing into `folder`, its first tick at
    /// `start`, and returns its id (`audio_exports_begun`): from here the
    /// clock's ticks play nothing, the run is stepped instead
    /// ([`next_audio_export_tick`](Self::next_audio_export_tick)), and the
    /// other threads see it running with its length. Remembers the loop state for
    /// [`finish_audio_export`](Self::finish_audio_export) to hand back.
    pub(crate) fn begin_audio_export(
        &mut self,
        plan: AudioExportPlan,
        folder: PathBuf,
        start: Instant,
    ) -> u32 {
        self.audio_exports_begun = self.audio_exports_begun.wrapping_add(1);
        let total_ms = ticks_to_millis_f64(self.tempo_us(), plan.end_tick - plan.start_tick);
        let atomics = &self.audio_export_atomics;
        atomics.written_ms.store(0, Ordering::Relaxed);
        atomics.total_ms.store(total_ms as u32, Ordering::Relaxed);
        atomics.running.store(true, Ordering::Relaxed);
        self.audio_export = Some(AudioExportRun {
            plan,
            folder,
            loop_was_enabled: self.is_loop_enabled(),
            cancelled: false,
            start,
            tempo_us: self.tempo_us(),
            ticks: 0,
            ended: false,
        });
        self.audio_exports_begun
    }

    /// The instant of the run's next tick, if it is due before `until`: the
    /// caller advances the transport and plays it ([`tick`](Self::tick)),
    /// and asks again. `None` once nothing more is due, and after the run's
    /// end mark: the transport stays on the selection's end while the mixer
    /// renders up to it. The caller has the run's seek land first (the
    /// `"sequencer"` thread's step arm).
    pub(crate) fn next_audio_export_tick(&self, until: Instant) -> Option<Instant> {
        let run = self.audio_export.as_ref()?;
        let at = run.now();
        (!run.ended && at < until).then_some(at)
    }

    /// Whether an export is running.
    pub(crate) fn is_exporting_audio(&self) -> bool {
        self.audio_export.is_some()
    }

    /// Ends the run and hands it back; `None` with none running. Sends no
    /// mark: [`close_audio_export`](Self::close_audio_export) does that.
    pub(crate) fn finish_audio_export(&mut self) -> Option<AudioExportRun> {
        let run = self.audio_export.take()?;
        self.audio_export_atomics
            .running
            .store(false, Ordering::Relaxed);
        Some(run)
    }

    /// Esc: closes the run ([`close_audio_export`](Self::close_audio_export))
    /// and marks it cancelled, so its files go once the writer is done. A
    /// no-op with no run.
    pub(crate) fn cancel_audio_export(&mut self) {
        self.close_audio_export();
        if let Some(run) = &mut self.audio_export {
            run.cancelled = true;
        }
    }

    /// Sends the run's missing marks, stamped with the run's own time now
    /// (the instant of its next tick, never the wall clock's), so the mixer
    /// ends the export there: the end, or with no start yet both. Nothing
    /// more is sent or stepped after. For Esc, and for a run the writer gave
    /// up on. A no-op with no run, or once both have gone.
    pub(crate) fn close_audio_export(&mut self) {
        let Some(run) = &mut self.audio_export else {
            return;
        };
        if run.ended {
            return;
        }
        let now = run.now();
        if run.ticks == 0 {
            let start = ExportMark::Start {
                id: self.audio_exports_begun,
                at: now,
                slots: run.plan.slots(),
            };
            self.export_mark_tx.push(start).ok();
        }
        self.export_mark_tx.push(ExportMark::End { at: now }).ok();
        run.ended = true;
    }

    /// Sends the mark for the tick the tracks play now, `at`, if it is the
    /// run's first (the selection's start) or the one a selection's length
    /// later (its end), and counts the tick. Counted in the run's own ticks,
    /// not on the transport's position: the files are the selection's length
    /// whatever else moves. During a run only its steps play ticks, so
    /// every tick counts until the end mark has gone.
    pub(super) fn mark_audio_export(&mut self, at: Instant) {
        let Some(run) = &mut self.audio_export else {
            return;
        };
        if run.ended {
            return;
        }
        let length = (run.plan.end_tick - run.plan.start_tick).max(0) as u64;
        let mark = if run.ticks == 0 {
            Some(ExportMark::Start {
                id: self.audio_exports_begun,
                at,
                slots: run.plan.slots(),
            })
        } else if run.ticks == length {
            run.ended = true;
            Some(ExportMark::End { at })
        } else {
            None
        };
        run.ticks += 1;
        if let Some(mark) = mark {
            self.export_mark_tx.push(mark).ok();
        }
    }
}

impl AudioExportPlan {
    /// The engine slots that get a stem, as the mixer indexes them.
    fn slots(&self) -> SlotMask {
        self.stems
            .iter()
            .fold(0, |mask, stem| mask | 1 << stem.slot)
    }
}

#[cfg(test)]
mod tests {
    use std::iter;
    use std::time::Duration;

    use rtrb::{Consumer, RingBuffer};

    use crate::core::sequencer::ClipInstrumentEvent;
    use crate::core::sequencer::test_support::{
        clip_at, clip_with_marker_notes, drain, rect, sequencer_with_outputs,
        step_audio_export as step, stub_instrument, test_sequencer,
    };
    use crate::core::time::Meter;

    use super::*;

    const BAR: i32 = Meter::FOUR_FOUR.bars_to_ticks(1);

    /// A sequencer whose first `plugin_tracks` tracks are plugin tracks, the
    /// rest MIDI Out, each with one clip over bars 1–2.
    fn sequencer_with_clips(plugin_tracks: usize) -> Sequencer {
        let mut sequencer = test_sequencer();
        for (position, track) in sequencer.tracks_mut().iter_mut().enumerate() {
            if position < plugin_tracks {
                track.set_output(stub_instrument(Vec::new()));
            }
            track.add_clip(&clip_at(0, 2 * BAR));
        }
        sequencer
    }

    /// The whole arrangement's first two bars, over track 0 only.
    fn two_bars() -> Option<TimeSelectionRect> {
        Some(rect(0, 2 * BAR, 0, 0))
    }

    fn stem_names(plan: &AudioExportPlan) -> Vec<&str> {
        plan.stems.iter().map(|s| s.file_name.as_str()).collect()
    }

    #[test]
    fn the_selection_sets_the_span_exactly() {
        let sequencer = sequencer_with_clips(1);
        let plan = sequencer
            .audio_export_plan(Some(rect(BAR / 2, 3 * BAR, 0, 0)))
            .unwrap();
        assert_eq!((plan.start_tick, plan.end_tick), (BAR / 2, 3 * BAR));
    }

    #[test]
    fn no_selection_refuses() {
        let sequencer = sequencer_with_clips(1);
        assert_eq!(
            sequencer.audio_export_plan(None),
            Err(AudioExportRefusal::NoSelection)
        );
        // A purely vertical marquee has no time to export either.
        assert_eq!(
            sequencer.audio_export_plan(Some(rect(BAR, BAR, 0, 3))),
            Err(AudioExportRefusal::NoSelection)
        );
    }

    #[test]
    fn every_plugin_track_gets_a_stem_and_midi_out_tracks_are_left_out() {
        let sequencer = sequencer_with_clips(2);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        // The selection covers track 0 only; the tracks still all count.
        assert_eq!(stem_names(&plan), ["01 Synth.wav", "02 Synth.wav"]);
        assert_eq!(
            plan.stems.iter().map(|s| s.slot).collect::<Vec<_>>(),
            [sequencer.tracks()[0].slot(), sequencer.tracks()[1].slot()]
        );
        let midi_out: Vec<usize> = (2..sequencer.tracks().len()).collect();
        assert_eq!(plan.left_out, midi_out);
    }

    #[test]
    fn a_named_track_names_its_stem() {
        let mut sequencer = sequencer_with_clips(1);
        sequencer.tracks_mut()[0].set_name(Some("Bass".into()));
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        assert_eq!(stem_names(&plan), ["01 Bass.wav"]);
    }

    #[test]
    fn mute_and_solo_choose_the_tracks() {
        let mut sequencer = sequencer_with_clips(3);
        sequencer.set_track_muted(0, true);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        assert_eq!(stem_names(&plan), ["02 Synth.wav", "03 Synth.wav"]);

        sequencer.set_track_soloed(2, true);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        assert_eq!(stem_names(&plan), ["03 Synth.wav"]);
        assert!(plan.left_out.is_empty(), "the MIDI Out tracks aren't heard");
    }

    #[test]
    fn a_track_with_no_unmuted_clip_in_the_selection_gets_no_stem() {
        let mut sequencer = sequencer_with_clips(3);
        // Track 1's only clip is muted; track 2's sits past the selection.
        let mut muted = clip_at(0, 2 * BAR);
        muted.set_muted(true);
        sequencer.tracks_mut()[1].clear_clips();
        sequencer.tracks_mut()[1].add_clip(&muted);
        sequencer.tracks_mut()[2].clear_clips();
        sequencer.tracks_mut()[2].add_clip(&clip_at(4 * BAR, BAR));
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        assert_eq!(stem_names(&plan), ["01 Synth.wav"]);
    }

    #[test]
    fn a_selection_that_only_touches_a_clip_still_counts_it() {
        let sequencer = sequencer_with_clips(1);
        let plan = sequencer
            .audio_export_plan(Some(rect(2 * BAR - 1, 4 * BAR, 0, 0)))
            .unwrap();
        assert_eq!(plan.stems.len(), 1);
        // Ending exactly where the clip starts doesn't.
        let mut later = test_sequencer();
        later.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
        later.tracks_mut()[0].add_clip(&clip_at(2 * BAR, BAR));
        assert_eq!(
            later.audio_export_plan(two_bars()),
            Err(AudioExportRefusal::NothingToHear)
        );
    }

    /// The instant tick `n` of a run from `origin` carries, at the test
    /// sequencer's tempo.
    fn tick_at(sequencer: &Sequencer, origin: Instant, n: u64) -> Instant {
        origin + Duration::from_nanos(ticks_to_nanos(sequencer.tempo_us(), n))
    }

    /// A one-plugin-track sequencer running an export of ticks 10–20 from
    /// `origin`, with the far end of its marks ring and the export's id.
    fn exporting(origin: Instant) -> (Sequencer, Consumer<ExportMark>, u32) {
        let mut sequencer = sequencer_with_clips(1);
        let (marks_tx, marks) = RingBuffer::new(4);
        sequencer.export_mark_tx = marks_tx;
        let plan = sequencer
            .audio_export_plan(Some(rect(10, 20, 0, 0)))
            .unwrap();
        let id = sequencer.begin_audio_export(plan, PathBuf::new(), origin);
        (sequencer, marks, id)
    }

    #[test]
    fn the_marks_go_out_on_the_runs_first_tick_and_a_selections_length_later() {
        let origin = Instant::now();
        let far = origin + Duration::from_secs(60);
        let (mut sequencer, mut marks, id) = exporting(origin);
        let slots = 1 << sequencer.tracks()[0].slot();

        // Ticks 0 to 10 of the run, the last carrying the end; then nothing
        // more is due, however far the render asks.
        assert_eq!(step(&mut sequencer, far), 11);
        assert_eq!(step(&mut sequencer, far), 0);
        let sent: Vec<ExportMark> = iter::from_fn(|| marks.pop().ok()).collect();
        assert_eq!(
            sent,
            [
                ExportMark::Start {
                    id,
                    at: origin,
                    slots
                },
                ExportMark::End {
                    at: tick_at(&sequencer, origin, 10)
                }
            ]
        );
    }

    #[test]
    fn a_step_plays_the_ticks_due_before_it_each_once() {
        let origin = Instant::now();
        let (mut sequencer, _marks, _) = exporting(origin);
        // The run's first tick is at its start, not before it.
        assert_eq!(step(&mut sequencer, origin), 0);
        let third = tick_at(&sequencer, origin, 3);
        assert_eq!(step(&mut sequencer, third), 3, "ticks 0, 1 and 2");
        assert_eq!(step(&mut sequencer, third), 0);
        assert_eq!(
            sequencer.next_audio_export_tick(third + Duration::from_nanos(1)),
            Some(third)
        );
    }

    #[test]
    fn a_stepped_run_sends_what_the_clock_would_have_at_the_same_instants() {
        let origin = Instant::now();
        let fixture = || {
            let (mut sequencer, _midi_out, plugin) = sequencer_with_outputs(false);
            sequencer.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
            sequencer.tracks_mut()[0].add_clip(&clip_with_marker_notes(0, 2 * BAR));
            sequencer.reset_to_tick(0);
            (sequencer, plugin)
        };
        let sent = |plugin: &mut Consumer<ClipInstrumentEvent>| -> Vec<_> {
            iter::from_fn(|| plugin.pop().ok())
                .map(|event| {
                    (
                        event.track,
                        event.message.midi(),
                        format!("{:?}", event.when),
                    )
                })
                .collect()
        };

        // Stepped: a block of 256 frames at 48 kHz at a time, as the render
        // asks, over the selection's bar.
        let (mut stepped, mut stepped_events) = fixture();
        let plan = stepped.audio_export_plan(Some(rect(0, BAR, 0, 0))).unwrap();
        stepped.begin_audio_export(plan, PathBuf::new(), origin);
        let block = Duration::from_nanos(256 * 1_000_000_000 / 48_000);
        let mut played = 0;
        for k in 0..2_000 {
            played += step(&mut stepped, origin + block * k);
        }
        assert_eq!(
            played,
            BAR as usize + 1,
            "each tick once, the end's included"
        );

        // Clocked: the same ticks, one call each, as the clock's would come.
        let (mut clocked, mut clocked_events) = fixture();
        for n in 0..=BAR as u64 {
            clocked.tick(tick_at(&clocked, origin, n));
        }

        let stepped_events = sent(&mut stepped_events);
        assert!(stepped_events.len() > 4, "the bar's notes");
        assert_eq!(stepped_events, sent(&mut clocked_events));
    }

    #[test]
    fn closing_before_the_start_sends_both_marks_at_the_runs_start() {
        let origin = Instant::now();
        let far = origin + Duration::from_secs(60);
        let (mut sequencer, mut marks, id) = exporting(origin);
        sequencer.close_audio_export();
        let slots = 1 << sequencer.tracks()[0].slot();
        let sent: Vec<ExportMark> = iter::from_fn(|| marks.pop().ok()).collect();
        assert_eq!(
            sent,
            [
                ExportMark::Start {
                    id,
                    at: origin,
                    slots
                },
                ExportMark::End { at: origin }
            ]
        );
        // Nothing more is stepped, and closing again sends nothing.
        assert_eq!(step(&mut sequencer, far), 0);
        sequencer.close_audio_export();
        assert!(marks.is_empty());
    }

    #[test]
    fn closing_after_the_start_sends_the_end_at_the_runs_own_time() {
        let origin = Instant::now();
        let (mut sequencer, mut marks, _) = exporting(origin);
        let third = tick_at(&sequencer, origin, 3);
        step(&mut sequencer, third);
        marks.pop().unwrap();
        sequencer.cancel_audio_export();
        // Three ticks were played: the next one's instant, not the wall
        // clock's.
        assert_eq!(marks.pop(), Ok(ExportMark::End { at: third }));
        assert!(marks.is_empty());
        assert!(sequencer.finish_audio_export().unwrap().cancelled);
    }

    #[test]
    fn closing_after_the_end_sends_nothing() {
        let origin = Instant::now();
        let (mut sequencer, mut marks, _) = exporting(origin);
        step(&mut sequencer, origin + Duration::from_secs(60));
        assert_eq!(iter::from_fn(|| marks.pop().ok()).count(), 2);
        sequencer.close_audio_export();
        assert!(marks.is_empty());
    }

    #[test]
    fn midi_out_tracks_send_nothing_while_an_export_renders() {
        let origin = Instant::now();
        let far = origin + Duration::from_secs(60);
        let (mut sequencer, midi_out, mut plugin) = sequencer_with_outputs(true);
        sequencer.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
        for track in sequencer.tracks_mut().iter_mut().take(2) {
            track.add_clip(&clip_with_marker_notes(0, BAR));
        }
        let plan = sequencer
            .audio_export_plan(Some(rect(0, BAR, 0, 0)))
            .unwrap();
        assert_eq!(plan.left_out, [1]);
        sequencer.begin_audio_export(plan, PathBuf::new(), origin);
        sequencer.reset_to_tick(0);
        sequencer.chase_notes(0);
        step(&mut sequencer, far);
        assert!(!drain(&mut plugin).is_empty(), "the plugin track plays");
        assert_eq!(midi_out.try_iter().count(), 0);

        // Ordinary playback of the same bar sends them.
        sequencer.finish_audio_export();
        sequencer.reset_to_tick(0);
        for tick in 0..BAR as u64 {
            sequencer.tick(tick_at(&sequencer, origin, tick));
        }
        assert!(midi_out.try_iter().count() > 0);
    }

    #[test]
    fn every_export_gets_its_own_id() {
        let (mut sequencer, _marks, first) = exporting(Instant::now());
        sequencer.finish_audio_export();
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        let second = sequencer.begin_audio_export(plan, PathBuf::new(), Instant::now());
        assert_ne!(second, first);
    }

    #[test]
    fn the_run_keeps_the_loop_state_it_found_and_shows_the_other_threads() {
        let mut sequencer = sequencer_with_clips(1);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        sequencer.begin_audio_export(plan, PathBuf::new(), Instant::now());
        assert!(sequencer.is_exporting_audio());
        let atomics = sequencer.audio_export_atomics.clone();
        assert!(atomics.is_running());
        // Two bars of 4/4 at the default 90 BPM: 16/3 s.
        assert_eq!(atomics.total_ms.load(Ordering::Relaxed), 5_333);
        let run = sequencer.finish_audio_export().unwrap();
        assert!(run.loop_was_enabled);
        assert!(!sequencer.is_exporting_audio());
        assert!(!atomics.is_running());
        assert!(sequencer.finish_audio_export().is_none());
    }

    #[test]
    fn nothing_to_hear_refuses() {
        // Only MIDI Out tracks.
        assert_eq!(
            sequencer_with_clips(0).audio_export_plan(two_bars()),
            Err(AudioExportRefusal::NothingToHear)
        );
        // A plugin track, but an empty project.
        let mut empty = test_sequencer();
        empty.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
        assert_eq!(
            empty.audio_export_plan(two_bars()),
            Err(AudioExportRefusal::NothingToHear)
        );
        // Every plugin track muted.
        let mut muted = sequencer_with_clips(1);
        muted.set_track_muted(0, true);
        assert_eq!(
            muted.audio_export_plan(two_bars()),
            Err(AudioExportRefusal::NothingToHear)
        );
    }
}
