//! The MIDI clip export (`⌘/Ctrl+⇧+E`): which clip goes out, and its `.mid`
//! bytes. The file write and the footer message are the handler's
//! (`060-persistence.md` § MIDI clip export).

use crate::core::{input_event::TimeSelectionRect, project::write_smf};

use super::Sequencer;

/// A clip ready to write: its `.mid` bytes, and where it sits (for the file
/// name).
pub(crate) struct ClipExport {
    /// Track index of the exported clip.
    pub(crate) track_idx: usize,
    /// The clip's arrangement start tick.
    pub(crate) start_tick: i32,
    /// The Standard MIDI File.
    pub(crate) bytes: Vec<u8>,
}

/// Why nothing was exported. Never silent: the handler says so in the
/// footer.
#[derive(Debug, PartialEq)]
pub(crate) enum ExportRefusal {
    /// The arranger marquee touches more than one clip.
    SeveralClips,
    /// There is no lead clip.
    NoClip,
}

impl Sequencer {
    /// The lead clip as a `.mid` ([`write_smf`]): the whole clip, whatever
    /// part of it the marquee or the note selection covers, as it plays
    /// ([`Clip::exported_events`](crate::models::clip::Clip::exported_events)),
    /// at the project tempo. Refused when `marquee` (the arranger's, `None`
    /// from the clip view) spans more than one clip on its tracks — the
    /// export never silently picks one of them.
    pub(crate) fn export_lead_clip(
        &self,
        marquee: Option<TimeSelectionRect>,
    ) -> Result<ClipExport, ExportRefusal> {
        if let Some(rect) = marquee.filter(TimeSelectionRect::has_tick_range)
            && self.clip_ids_in(rect).nth(1).is_some()
        {
            return Err(ExportRefusal::SeveralClips);
        }

        let (Some(track_idx), Some(clip)) = (self.selected_track_index(), self.selected_clip())
        else {
            return Err(ExportRefusal::NoClip);
        };
        Ok(ClipExport {
            track_idx,
            start_tick: clip.start_tick(),
            bytes: write_smf(
                &clip.exported_events(),
                clip.region_length(),
                self.tempo_us(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::core::sequencer::test_support::{clip_at, note_off, note_on, rect, test_sequencer};
    use crate::models::clip::Clip;

    use super::*;

    /// A sequencer with `clips` on track 0, track 0 selected, the first clip
    /// the lead.
    fn sequencer_with_clips(clips: &[Clip]) -> Sequencer {
        let mut sequencer = test_sequencer();
        for clip in clips {
            sequencer.tracks_mut()[0].add_clip(clip);
        }
        let track_id = sequencer.tracks()[0].id();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(clips.first().map(Clip::id));
        sequencer
    }

    #[test]
    fn exports_the_lead_clip_at_its_length() {
        let mut clip = clip_at(1920, 3840);
        clip.add_event(note_on(0));
        clip.add_event(note_off(480));
        clip.calculate_note_lengths();
        let sequencer = sequencer_with_clips(&[clip.clone()]);

        let export = sequencer.export_lead_clip(None).unwrap();

        assert_eq!((export.track_idx, export.start_tick), (0, 1920));
        assert_eq!(
            export.bytes,
            write_smf(&clip.exported_events(), 3840, sequencer.tempo_us())
        );
    }

    #[test]
    fn a_marquee_inside_one_clip_still_exports_the_whole_clip() {
        let sequencer = sequencer_with_clips(&[clip_at(0, 3840)]);
        let whole = sequencer.export_lead_clip(None).unwrap().bytes;

        let export = sequencer.export_lead_clip(Some(rect(960, 1920, 0, 0)));

        assert_eq!(export.unwrap().bytes, whole);
    }

    #[test]
    fn a_marquee_over_two_clips_is_refused() {
        let sequencer = sequencer_with_clips(&[clip_at(0, 960), clip_at(1920, 960)]);
        let export = sequencer.export_lead_clip(Some(rect(0, 3840, 0, 0)));
        assert_eq!(export.err(), Some(ExportRefusal::SeveralClips));
    }

    #[test]
    fn a_marquee_over_clips_on_two_tracks_is_refused() {
        let mut sequencer = sequencer_with_clips(&[clip_at(0, 960)]);
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960));
        let export = sequencer.export_lead_clip(Some(rect(0, 960, 0, 1)));
        assert_eq!(export.err(), Some(ExportRefusal::SeveralClips));
    }

    #[test]
    fn a_track_only_marquee_is_no_selection() {
        let sequencer = sequencer_with_clips(&[clip_at(0, 960), clip_at(1920, 960)]);
        assert!(sequencer.export_lead_clip(Some(rect(0, 0, 0, 0))).is_ok());
    }

    #[test]
    fn no_lead_clip_is_refused() {
        let sequencer = sequencer_with_clips(&[]);
        assert_eq!(
            sequencer.export_lead_clip(None).err(),
            Some(ExportRefusal::NoClip)
        );
    }
}
