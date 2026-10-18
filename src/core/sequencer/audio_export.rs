//! The audio export's plan (`⌘/Ctrl+⇧+R`, `300-export-to-audio.md`): which
//! tracks get a stem, what each file is called, and the stretch of time it
//! covers. The run that plays it and the files it writes are the handler's.

use crate::{
    core::{input_event::TimeSelectionRect, project::stem_file_name},
    models::track::Track,
};

use super::Sequencer;

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

/// What a stem is named after: the track's own name, or for a track never
/// named, its plugin's (`plugin_name`) — the header's bare number would say
/// nothing in a DAW that the file's `NN` doesn't already.
fn stem_name<'a>(track: &'a Track, plugin_name: &'a str) -> &'a str {
    track.name().unwrap_or(plugin_name)
}

#[cfg(test)]
mod tests {
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
