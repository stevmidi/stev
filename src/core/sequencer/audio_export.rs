//! The audio export (`⌘/Ctrl+⇧+R`, `300-export-to-audio.md`), its sequencer
//! half: the plan (which tracks get a stem, what each file is called, the
//! stretch of time it covers) and the run's state while it plays. The run
//! sends the [`ExportMark`]s from the tick path, stamped with the `Instant`
//! of the tick the tracks play at the selection's start and end, so the
//! mixer cuts every file where those ticks sound. The workflow that starts
//! and ends the run is the handler's; the tap and the files are the plugin
//! host's.

use std::path::PathBuf;
use std::time::Instant;

use crate::{
    core::{config::MAX_TRACKS, input_event::TimeSelectionRect, project::stem_file_name},
    models::track::Track,
};

use super::Sequencer;

/// One end of the export, from the sequencer to the plugin host's mixer on
/// its own `rtrb` ring. The mixer turns `at` into a frame the way it places
/// a note-on (`RenderCtx::scheduled_frame`), so the files start and end on
/// the samples where those ticks sound.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum ExportMark {
    /// The selection's start tick: sample 0 of every file.
    Start {
        /// The tick's intended time.
        at: Instant,
        /// The engine slots that get a stem ([`Track::slot`]).
        slots: [bool; MAX_TRACKS],
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
    /// Each stem's engine slot and file.
    pub(crate) stems: Vec<(usize, PathBuf)>,
    /// The mix file.
    pub(crate) mix: PathBuf,
}

/// How the `"audio-export"` thread's files came out, sent back once it has
/// finished them (`SequencerCommand::AudioExportWritten`).
#[derive(Debug, Default, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct AudioExportOutcome {
    /// The first file error; what was written before it stays on disk.
    pub(crate) error: Option<String>,
    /// The stem slots with no plugin loaded: their files are silence.
    pub(crate) silent: Vec<usize>,
    /// A ring filled before the writer drained it, so samples were dropped.
    pub(crate) overflowed: bool,
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
    /// The tick the tracks play on the next [`Sequencer::tick`], counted
    /// from the last re-seek. `None` until the run's own seek.
    position: Option<i32>,
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
    /// The track's engine slot ([`Track::slot`]): the voice the mixer taps.
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
                    file_name: stem_file_name(
                        position + 1,
                        stem_name(track, &instrument.display_name),
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
}

impl Sequencer {
    /// Starts the run for `plan`, writing into `folder`: from here the tick
    /// path sends its marks. Remembers the loop state for
    /// [`finish_audio_export`](Self::finish_audio_export) to hand back.
    pub(crate) fn begin_audio_export(&mut self, plan: AudioExportPlan, folder: PathBuf) {
        self.audio_export = Some(AudioExportRun {
            plan,
            folder,
            loop_was_enabled: self.is_loop_enabled(),
            position: None,
        });
    }

    /// Whether an export is running.
    pub(crate) fn is_exporting_audio(&self) -> bool {
        self.audio_export.is_some()
    }

    /// Ends the run and hands it back; `None` with none running.
    pub(crate) fn finish_audio_export(&mut self) -> Option<AudioExportRun> {
        self.audio_export.take()
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
        let mark = if *tick == run.plan.start_tick {
            Some(ExportMark::Start {
                at,
                slots: run.plan.slots(),
            })
        } else if *tick == run.plan.end_tick {
            Some(ExportMark::End { at })
        } else {
            None
        };
        *tick += 1;
        if let Some(mark) = mark {
            self.export_mark_tx.push(mark).ok();
        }
    }
}

impl AudioExportPlan {
    /// The engine slots that get a stem, as the mixer indexes them.
    fn slots(&self) -> [bool; MAX_TRACKS] {
        let mut slots = [false; MAX_TRACKS];
        for stem in &self.stems {
            slots[stem.slot] = true;
        }
        slots
    }
}

/// What a stem is named after: the track's own name, or for a track never
/// named, its plugin's (`plugin_name`) — the header's bare number would say
/// nothing in a DAW that the file's `NN` doesn't already.
fn stem_name<'a>(track: &'a Track, plugin_name: &'a str) -> &'a str {
    track.name().unwrap_or(plugin_name)
}

#[cfg(test)]
mod tests {
    use std::iter;
    use std::time::Duration;

    use rtrb::RingBuffer;

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
        let mut slots = [false; MAX_TRACKS];
        slots[sequencer.tracks()[0].slot()] = true;
        sequencer.begin_audio_export(plan, PathBuf::new());

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
                ExportMark::Start { at: at(10), slots },
                ExportMark::End { at: at(20) }
            ]
        );
    }

    #[test]
    fn the_run_keeps_the_loop_state_it_found() {
        let mut sequencer = sequencer_with_clips(1);
        let plan = sequencer.audio_export_plan(two_bars()).unwrap();
        sequencer.begin_audio_export(plan, PathBuf::new());
        assert!(sequencer.is_exporting_audio());
        let run = sequencer.finish_audio_export().unwrap();
        assert!(run.loop_was_enabled);
        assert!(!sequencer.is_exporting_audio());
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
