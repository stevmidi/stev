//! The audio export (`⌘/Ctrl+⇧+R`, `130-plugin-host.md` § Audio export), its sequencer
//! half: the plan (which tracks get a stem, what each file is called, the
//! stretch of time it covers) and the run's state while it plays. The run
//! sends the [`ExportMark`]s from the tick path, stamped with the `Instant`
//! of the tick the tracks play at the selection's start and end, so the
//! mixer cuts every file where those ticks sound. The workflow that starts
//! and ends the run is the handler's; the tap and the files are the plugin
//! host's.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::core::{
    config::MAX_TRACKS, input_event::TimeSelectionRect, project::stem_file_name,
    time::ticks_to_millis_f64,
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

/// The files one export writes, sent to the `"audio-export"` thread before
/// the run plays. The folder already exists.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct AudioExportJob {
    /// The export's id, as its start mark carries it.
    pub(crate) id: u32,
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
    /// the audio output changed or stopped, so the files are wrong.
    pub(crate) stopped: Option<&'static str>,
    /// The first file error; what was written before it stays on disk.
    pub(crate) error: Option<String>,
    /// The stem slots with no plugin for some or all of the run: their files
    /// are silence there.
    pub(crate) silent: Vec<usize>,
    /// A ring filled before the writer drained it, so samples were dropped.
    pub(crate) overflowed: bool,
    /// Audio blocks that missed their deadline during the run
    /// (`AudioLoad::overruns`).
    pub(crate) overruns: u32,
    /// The audio device's own overload reports during the run
    /// (`AudioLoad::xruns`).
    pub(crate) xruns: u32,
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
    /// The tick the tracks play on the next [`Sequencer::tick`], counted
    /// from the last re-seek. `None` until the run's own seek.
    position: Option<i32>,
    /// Which of its marks have gone to the mixer.
    sent: MarksSent,
}

/// How far an export's marks have gone: start, then end, each once.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MarksSent {
    /// Neither yet.
    None,
    /// The start mark.
    Start,
    /// Both: the mixer will end the export.
    End,
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

    /// Starts the run for `plan`, writing into `folder`, and returns its id
    /// (`audio_exports_begun`): from here the tick path sends its marks, and
    /// the other threads see it running with its length. Remembers the loop
    /// state for [`finish_audio_export`](Self::finish_audio_export) to hand
    /// back.
    pub(crate) fn begin_audio_export(&mut self, plan: AudioExportPlan, folder: PathBuf) -> u32 {
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
            position: None,
            sent: MarksSent::None,
        });
        self.audio_exports_begun
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
    pub(crate) fn cancel_audio_export(&mut self, now: Instant) {
        self.close_audio_export(now);
        if let Some(run) = &mut self.audio_export {
            run.cancelled = true;
        }
    }

    /// Sends the run's missing marks, stamped `now`, so the mixer ends the
    /// export there whatever the transport does next: the end, or with no
    /// start yet both. Nothing more is sent after. For Esc, and for a run the
    /// writer gave up on. A no-op with no run, or once both have gone.
    pub(crate) fn close_audio_export(&mut self, now: Instant) {
        let Some(run) = &mut self.audio_export else {
            return;
        };
        if run.sent == MarksSent::None {
            let start = ExportMark::Start {
                id: self.audio_exports_begun,
                at: now,
                slots: run.plan.slots(),
            };
            self.export_mark_tx.push(start).ok();
        }
        if run.sent != MarksSent::End {
            self.export_mark_tx.push(ExportMark::End { at: now }).ok();
        }
        run.sent = MarksSent::End;
    }

    /// The tracks were re-seeked to `tick` (`reset_to_tick`).
    pub(super) fn seek_audio_export(&mut self, tick: i32) {
        if let Some(run) = &mut self.audio_export {
            run.position = Some(tick);
        }
    }

    /// Sends the mark for the tick the tracks play now, `at`, if it is the
    /// selection's start or end. Counted on the tracks' own position, not
    /// the transport's: a clock tick that slips in before the run's seek
    /// reaches the tracks plays where they were, and must not start the
    /// files.
    pub(super) fn mark_audio_export(&mut self, at: Instant) {
        let Some(run) = &mut self.audio_export else {
            return;
        };
        let Some(tick) = run.position.as_mut() else {
            return;
        };
        let mark = match run.sent {
            MarksSent::None if *tick == run.plan.start_tick => {
                run.sent = MarksSent::Start;
                Some(ExportMark::Start {
                    id: self.audio_exports_begun,
                    at,
                    slots: run.plan.slots(),
                })
            }
            MarksSent::Start if *tick == run.plan.end_tick => {
                run.sent = MarksSent::End;
                Some(ExportMark::End { at })
            }
            _ => None,
        };
        *tick += 1;
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

    use crate::core::sequencer::test_support::{clip_at, rect, stub_instrument, test_sequencer};
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

    #[test]
    fn the_marks_go_out_on_the_ticks_the_tracks_play_at_the_selections_ends() {
        let mut sequencer = sequencer_with_clips(1);
        let (marks_tx, mut marks) = RingBuffer::new(4);
        sequencer.export_mark_tx = marks_tx;
        let plan = sequencer
            .audio_export_plan(Some(rect(10, 20, 0, 0)))
            .unwrap();
        let slots = 1 << sequencer.tracks()[0].slot();
        let id = sequencer.begin_audio_export(plan, PathBuf::new());

        let origin = Instant::now();
        let at = |tick: i32| origin + Duration::from_millis(tick as u64);
        // Before the run's seek reaches the tracks, nothing counts.
        sequencer.tick(at(0));
        sequencer.reset_to_tick(8);
        for tick in 8..30 {
            sequencer.tick(at(tick));
        }

        let sent: Vec<ExportMark> = iter::from_fn(|| marks.pop().ok()).collect();
        assert_eq!(
            sent,
            [
                ExportMark::Start {
                    id,
                    at: at(10),
                    slots
                },
                ExportMark::End { at: at(20) }
            ]
        );

        // A seek back over the selection sends nothing more.
        sequencer.reset_to_tick(8);
        for tick in 8..30 {
            sequencer.tick(at(tick));
        }
        assert!(marks.is_empty());
    }

    /// A one-plugin-track sequencer running an export of ticks 10–20, with
    /// the far end of its marks ring and the export's id.
    fn exporting() -> (Sequencer, Consumer<ExportMark>, u32) {
        let mut sequencer = sequencer_with_clips(1);
        let (marks_tx, marks) = RingBuffer::new(4);
        sequencer.export_mark_tx = marks_tx;
        let plan = sequencer
            .audio_export_plan(Some(rect(10, 20, 0, 0)))
            .unwrap();
        let id = sequencer.begin_audio_export(plan, PathBuf::new());
        sequencer.reset_to_tick(10);
        (sequencer, marks, id)
    }

    #[test]
    fn closing_before_the_start_sends_both_marks_now() {
        let (mut sequencer, mut marks, id) = exporting();
        let now = Instant::now();
        sequencer.close_audio_export(now);
        let slots = 1 << sequencer.tracks()[0].slot();
        let sent: Vec<ExportMark> = iter::from_fn(|| marks.pop().ok()).collect();
        assert_eq!(
            sent,
            [
                ExportMark::Start { id, at: now, slots },
                ExportMark::End { at: now }
            ]
        );
        // The ticks that follow send nothing, and closing again neither.
        sequencer.tick(now);
        sequencer.close_audio_export(now);
        assert!(marks.is_empty());
    }

    #[test]
    fn closing_after_the_start_sends_the_end_now() {
        let (mut sequencer, mut marks, _) = exporting();
        let origin = Instant::now();
        sequencer.tick(origin);
        marks.pop().unwrap();
        let now = origin + Duration::from_millis(3);
        sequencer.cancel_audio_export(now);
        assert_eq!(marks.pop(), Ok(ExportMark::End { at: now }));
        assert!(marks.is_empty());
        assert!(sequencer.finish_audio_export().unwrap().cancelled);
    }

    #[test]
    fn closing_after_the_end_sends_nothing() {
        let (mut sequencer, mut marks, _) = exporting();
        let origin = Instant::now();
        for tick in 0..11 {
            sequencer.tick(origin + Duration::from_millis(tick));
        }
        assert_eq!(iter::from_fn(|| marks.pop().ok()).count(), 2);
        sequencer.close_audio_export(origin);
        assert!(marks.is_empty());
    }

    #[test]
    fn every_export_gets_its_own_id() {
        let (mut sequencer, _marks, first) = exporting();
        sequencer.finish_audio_export();
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        assert_ne!(sequencer.begin_audio_export(plan, PathBuf::new()), first);
    }

    #[test]
    fn the_run_keeps_the_loop_state_it_found_and_shows_the_other_threads() {
        let mut sequencer = sequencer_with_clips(1);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        sequencer.begin_audio_export(plan, PathBuf::new());
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
