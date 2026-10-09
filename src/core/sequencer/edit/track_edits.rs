//! Adding, removing and renaming tracks: [`AddTrackEdit`] (⌘T, the `+`
//! row), [`RemoveTrackEdit`] (Delete in track header focus, the output menu's
//! `Delete track`) and [`RenameTrackEdit`] (⌘R, a double-click on the
//! header's name), each one undo step.
//!
//! They are mirror images built on one pair of helpers: [`Sequencer::lift_track`]
//! takes a track out of the arrangement whole (its clips, output and mix
//! values, as a [`LiftedTrack`]) and [`Sequencer::restore_track`] puts one
//! back at a position. Undo has to work: every other edit names its track by
//! position, and the record stays sound only because an add or remove is undone
//! in reverse order like any other step, putting the track back where the
//! later steps expect it.
//!
//! Engine state is keyed by [`Track::slot`], which a track keeps across a lift
//! and restore, so nothing moves on the audio thread. A plugin voice is
//! never parked in the undo record: the lifted track keeps only the plugin's
//! `InstrumentRef`, `Display` tears the live plugin down (keeping its live
//! state by track id), and a restore reloads it the way a project load does.
//! See `050-undo-redo.md` § Track add / remove.

use crate::core::config::{MAX_TRACKS, TRACK_COLOR_COUNT};
use crate::models::track::{Track, TrackOutput};

use super::{EditResult, Sequencer};

/// A track out of the arrangement — removed, or an added one undone — with
/// the mix values its slot held, ready for [`Sequencer::restore_track`].
pub(crate) struct LiftedTrack {
    /// The whole track: clips, output (with the plugin's state blob), id and
    /// slot.
    track: Track,
    /// Its volume, dB.
    volume_db: f32,
    /// Its stereo balance.
    pan: f32,
    /// Its mute flag.
    muted: bool,
    /// Its solo flag.
    soloed: bool,
}

impl LiftedTrack {
    /// A new empty MIDI-Out track on `channel`, in colour `color_slot`, at
    /// neutral mix. Its slot is picked when it is restored.
    fn fresh(channel: u8, color_slot: usize) -> Self {
        LiftedTrack {
            track: Track::new(TrackOutput::MidiOut { channel }, 0, color_slot),
            volume_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
        }
    }
}

impl Sequencer {
    /// Takes the track at `track_idx` out of the arrangement: silences what it
    /// has sounding (while running), puts its slot's mix back at neutral and
    /// returns it with its mix values. Leaves the track selection as it is
    /// (possibly naming the lifted track) — the handler selects the track
    /// that took its place. `None` out of range or for the last track (a
    /// project never has zero).
    pub(crate) fn lift_track(&mut self, track_idx: usize) -> Option<LiftedTrack> {
        if track_idx >= self.tracks.len() || self.tracks.len() <= 1 {
            return None;
        }
        if self.is_running() {
            self.silence_leaving_track(track_idx);
        }
        let lifted = LiftedTrack {
            volume_db: self.track_volume_db(track_idx),
            pan: self.track_pan(track_idx),
            muted: self.track_muted(track_idx),
            soloed: self.track_soloed(track_idx),
            track: self.tracks.remove(track_idx),
        };
        self.reset_slot_mix(lifted.track.slot());
        Some(lifted)
    }

    /// Puts `lifted` back into the arrangement at `track_idx` (clamped to an
    /// append): in its own slot when that is free, else the lowest free one;
    /// caught up to the playhead; its mix values written back. Returns where it landed. Never
    /// grows past `MAX_TRACKS` (`None`).
    pub(crate) fn restore_track(&mut self, track_idx: usize, lifted: LiftedTrack) -> Option<usize> {
        if self.tracks.len() >= MAX_TRACKS {
            return None;
        }
        let LiftedTrack {
            mut track,
            volume_db,
            pan,
            muted,
            soloed,
        } = lifted;
        if self.tracks.iter().any(|t| t.slot() == track.slot()) {
            track.set_slot(self.free_slot()?);
        }
        track.seek(self.playback_tick());

        let track_idx = track_idx.min(self.tracks.len());
        self.tracks.insert(track_idx, track);
        self.set_track_volume(track_idx, volume_db);
        self.set_track_pan(track_idx, pan);
        self.set_track_muted(track_idx, muted);
        self.set_track_soloed(track_idx, soloed);
        // A restored solo silences the others.
        if soloed {
            self.release_newly_silenced_tracks();
        }
        Some(track_idx)
    }

    /// The lowest MIDI channel no MIDI-Out track uses — a new track's. Always
    /// one free with at most `MAX_TRACKS` (16) tracks; channel 1 otherwise.
    pub(crate) fn lowest_unused_channel(&self) -> u8 {
        (0..16u8)
            .find(|&channel| {
                !self.tracks.iter().any(
                    |t| matches!(t.output(), TrackOutput::MidiOut { channel: c } if *c == channel),
                )
            })
            .unwrap_or(0)
    }

    /// Lifts the track at `track_idx` into `keep` — the removing half both
    /// edits share (`RemoveTrackEdit::edit`, `AddTrackEdit::undo`).
    fn lift_into(&mut self, track_idx: usize, keep: &mut Option<LiftedTrack>) -> EditResult {
        let Some(lifted) = self.lift_track(track_idx) else {
            return EditResult::NoOp;
        };
        let result = EditResult::TrackRemoved {
            track_idx,
            track_id: lifted.track.id(),
            slot: lifted.track.slot(),
        };
        *keep = Some(lifted);
        result
    }

    /// Restores `lifted` at `track_idx` — the adding half both edits share
    /// (`AddTrackEdit::edit`, `RemoveTrackEdit::undo`).
    fn restore_into(&mut self, track_idx: usize, lifted: LiftedTrack) -> EditResult {
        let Some(track_idx) = self.restore_track(track_idx, lifted) else {
            return EditResult::NoOp;
        };
        let track = &self.tracks[track_idx];
        let instrument = match track.output() {
            TrackOutput::Instrument(instrument) => Some(instrument.clone()),
            TrackOutput::MidiOut { .. } => None,
        };
        EditResult::TrackAdded {
            track_idx,
            track_id: track.id(),
            slot: track.slot(),
            instrument,
        }
    }
}

/// ⌘T / the `+` row: a new empty MIDI track after the selected one (or at
/// a given position), on the lowest MIDI channel no other MIDI-Out track
/// uses, in the next colour in rotation ([`next_color_slot`]). Undo lifts it
/// again — whatever has been done to it since (a plugin, a mix) — and redo
/// puts that back.
pub(crate) struct AddTrackEdit {
    /// Where the track goes.
    track_idx: usize,
    /// Its MIDI channel, fixed when the edit is made.
    channel: u8,
    /// Its colour, fixed when the edit is made.
    color_slot: usize,
    /// The track as undo lifted it, for redo to put back; `None` before the
    /// first undo (redo then — the first edit — makes a fresh track).
    lifted: Option<LiftedTrack>,
}

impl AddTrackEdit {
    /// A track at `track_idx` (clamped to an append), or right after the
    /// selected one when `None` (at the end with none selected). `None` at
    /// `MAX_TRACKS`.
    pub(crate) fn new(sequencer: &Sequencer, track_idx: Option<usize>) -> Option<Self> {
        let count = sequencer.tracks().len();
        let track_idx = track_idx
            .or_else(|| sequencer.selected_track_index().map(|idx| idx + 1))
            .map_or(count, |idx| idx.min(count));
        (count < MAX_TRACKS).then(|| AddTrackEdit {
            track_idx,
            channel: sequencer.lowest_unused_channel(),
            color_slot: next_color_slot(
                &sequencer
                    .tracks()
                    .iter()
                    .map(Track::color_slot)
                    .collect::<Vec<_>>(),
            ),
            lifted: None,
        })
    }

    /// Puts the track in (fresh, or as undo left it).
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        let lifted = self
            .lifted
            .take()
            .unwrap_or_else(|| LiftedTrack::fresh(self.channel, self.color_slot));
        sequencer.restore_into(self.track_idx, lifted)
    }

    /// Takes the track back out, keeping it for redo.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.lift_into(self.track_idx, &mut self.lifted)
    }
}

/// Delete in track header focus / the output menu's `Delete track`: removes a track — its clips,
/// plugin and mix values. Undo puts it back at its position; a plugin track's
/// plugin reloads from its state blob.
pub(crate) struct RemoveTrackEdit {
    /// The track's position.
    track_idx: usize,
    /// The track as the edit lifted it, for undo; `None` while it is in the
    /// arrangement.
    lifted: Option<LiftedTrack>,
}

impl RemoveTrackEdit {
    /// Removes the track at `track_idx`, or the selected one when `None`.
    /// `None` with no such track, or on the last remaining track.
    pub(crate) fn new(sequencer: &Sequencer, track_idx: Option<usize>) -> Option<Self> {
        let track_idx = track_idx.or_else(|| sequencer.selected_track_index())?;
        (track_idx < sequencer.tracks().len() && sequencer.tracks().len() > 1).then_some(
            RemoveTrackEdit {
                track_idx,
                lifted: None,
            },
        )
    }

    /// Lifts the track out, keeping it for undo.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.lift_into(self.track_idx, &mut self.lifted)
    }

    /// Puts the track back at its position.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        match self.lifted.take() {
            Some(lifted) => sequencer.restore_into(self.track_idx, lifted),
            None => EditResult::NoOp,
        }
    }
}

/// ⌘R / a double-click on the header's name: names a track, or (`None`)
/// takes its name away so the header shows its number again.
pub(crate) struct RenameTrackEdit {
    /// The track's position.
    track_idx: usize,
    /// Its name before the edit.
    before: Option<String>,
    /// Its name after.
    after: Option<String>,
}

impl RenameTrackEdit {
    /// Names the track at `track_idx` `name` (already through
    /// `track_name_from_input`). `None` with no such track, or when it
    /// already has that name — no undo step for a rename that changes
    /// nothing.
    pub(crate) fn new(
        sequencer: &Sequencer,
        track_idx: usize,
        name: Option<String>,
    ) -> Option<Self> {
        let before = sequencer.tracks().get(track_idx)?.name();
        (before != name.as_deref()).then(|| RenameTrackEdit {
            track_idx,
            before: before.map(str::to_owned),
            after: name,
        })
    }

    /// Gives the track its new name.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.rename_track(self.track_idx, self.after.clone())
    }

    /// Gives it back its old one.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.rename_track(self.track_idx, self.before.clone())
    }
}

impl Sequencer {
    /// Names the track at `track_idx` — both halves of [`RenameTrackEdit`].
    fn rename_track(&mut self, track_idx: usize, name: Option<String>) -> EditResult {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return EditResult::NoOp;
        };
        track.set_name(name);
        EditResult::TrackRenamed
    }
}

/// The colour a new track takes, given the tracks' colours in order: the
/// first in rotation from just past the last track's that no track uses, so
/// adding keeps walking the palette and a colour freed by a remove comes back
/// only once the rotation wraps round to it. Past the last track's when every
/// colour is taken (only with more tracks than `TRACK_COLOR_COUNT`).
fn next_color_slot(colors: &[usize]) -> usize {
    let after = colors.last().map_or(0, |&last| last + 1);
    (0..TRACK_COLOR_COUNT)
        .map(|step| (after + step) % TRACK_COLOR_COUNT)
        .find(|color| !colors.contains(color))
        .unwrap_or(after % TRACK_COLOR_COUNT)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use crossbeam_channel::Receiver;
    use rtrb::Consumer;
    use undo::Record;
    use uuid::Uuid;

    use crate::core::config::{MAX_TRACKS, TRACK_COLOR_COUNT};
    use crate::core::midi::out_queue::MidiOutMessage;
    use crate::core::sequencer::test_support::{
        clip_at, note_off, note_on, sequencer_with_outputs, stub_instrument, test_sequencer,
        track_colors,
    };
    use crate::core::sequencer::{
        ClipInstrumentEvent, DeleteInRangeEdit, EditResult, Sequencer, SequencerEdit,
    };
    use crate::models::track::TrackOutput;

    use super::{AddTrackEdit, RemoveTrackEdit, RenameTrackEdit, next_color_slot};

    /// The tracks' ids, in order.
    fn ids(sequencer: &Sequencer) -> Vec<Uuid> {
        sequencer.tracks().iter().map(|t| t.id()).collect()
    }

    /// The tracks' engine slots, in order.
    fn slots(sequencer: &Sequencer) -> Vec<usize> {
        sequencer.tracks().iter().map(|t| t.slot()).collect()
    }

    /// Selects the track at `track_idx`.
    fn select(sequencer: &mut Sequencer, track_idx: usize) {
        let id = sequencer.track_id_by_index(track_idx);
        sequencer.select_track(id);
    }

    /// A running sequencer, both output ends, with a middle-C note `[0, 240)`
    /// in a clip on `track_idx`, played one tick in so it is sounding.
    fn sounding_note_on(
        track_idx: usize,
        output: Option<TrackOutput>,
    ) -> (
        Sequencer,
        Receiver<MidiOutMessage>,
        Consumer<ClipInstrumentEvent>,
    ) {
        let (mut sequencer, midi_out_rx, plugin_rx) = sequencer_with_outputs(false);
        if let Some(output) = output {
            sequencer.tracks_mut()[track_idx].set_output(output);
        }
        let mut clip = clip_at(0, 960);
        clip.add_event(note_on(0));
        clip.add_event(note_off(240));
        sequencer.tracks_mut()[track_idx].add_clip(&clip);
        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(0);
        sequencer.tick(Instant::now());
        (sequencer, midi_out_rx, plugin_rx)
    }

    /// Every message the MIDI-out receiver has, as bytes.
    fn midi_out(rx: &Receiver<MidiOutMessage>) -> Vec<Vec<u8>> {
        rx.try_iter().map(|m| m.bytes).collect()
    }

    /// Every plugin-bound event, as `(slot, message)`.
    fn plugin_events(rx: &mut Consumer<ClipInstrumentEvent>) -> Vec<(usize, [u8; 3])> {
        std::iter::from_fn(|| rx.pop().ok())
            .map(|e| (e.track, e.message))
            .collect()
    }

    #[test]
    fn add_puts_an_empty_midi_track_after_the_selected_one_on_the_lowest_free_channel() {
        let mut sequencer = test_sequencer();
        // Channels in use: 1, 2, 10, 4 — the lowest free one is 3.
        sequencer.set_track_output(2, TrackOutput::MidiOut { channel: 9 });
        select(&mut sequencer, 1);
        let before = ids(&sequencer);
        let mut record = Record::new();

        let edit = AddTrackEdit::new(&sequencer, None).unwrap();
        let result = record.edit(&mut sequencer, SequencerEdit::from(edit));

        assert!(matches!(
            result,
            EditResult::TrackAdded {
                track_idx: 2,
                slot: 4,
                instrument: None,
                ..
            }
        ));
        assert_eq!(sequencer.tracks().len(), 5);
        let added = &sequencer.tracks()[2];
        assert!(added.clips().is_empty());
        assert!(matches!(
            added.output(),
            TrackOutput::MidiOut { channel: 2 }
        ));
        // The old tracks keep their slots; the new one takes the free slot.
        assert_eq!(slots(&sequencer), vec![0, 1, 4, 2, 3]);
        let added_id = added.id();

        record.undo(&mut sequencer);
        assert_eq!(ids(&sequencer), before);

        // Redo brings the same track back, not a new one.
        record.redo(&mut sequencer);
        assert_eq!(sequencer.tracks()[2].id(), added_id);
        assert_eq!(sequencer.tracks().len(), 5);
    }

    #[test]
    fn add_with_no_track_selected_and_at_end_both_append() {
        let mut sequencer = test_sequencer();
        let mut edit = AddTrackEdit::new(&sequencer, None).unwrap();
        edit.edit(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), 5);
        assert!(matches!(
            sequencer.tracks()[4].output(),
            TrackOutput::MidiOut { channel: 4 }
        ));

        select(&mut sequencer, 0);
        let mut edit = AddTrackEdit::new(&sequencer, Some(sequencer.tracks().len())).unwrap();
        assert!(matches!(
            edit.edit(&mut sequencer),
            EditResult::TrackAdded { track_idx: 5, .. }
        ));
    }

    #[test]
    fn next_color_walks_on_past_the_last_tracks_skipping_used_ones() {
        assert_eq!(next_color_slot(&[0, 1, 2, 3]), 4);
        // An insert mid-list doesn't restart the rotation.
        assert_eq!(next_color_slot(&[0, 4, 1, 2, 3]), 5);
        // A colour freed by a remove waits for the rotation to wrap.
        assert_eq!(next_color_slot(&[0, 1, 3, 4, 5]), 6);
        assert_eq!(next_color_slot(&[3, 14, 15]), 0);
        assert_eq!(next_color_slot(&[0, 15]), 1);
        assert_eq!(next_color_slot(&[]), 0);
    }

    #[test]
    fn next_color_repeats_only_when_every_colour_is_taken() {
        let all: Vec<usize> = (0..TRACK_COLOR_COUNT).collect();
        assert_eq!(next_color_slot(&all), 0);
    }

    #[test]
    fn tracks_keep_their_colour_across_add_remove_undo_and_redo() {
        let mut sequencer = test_sequencer();
        select(&mut sequencer, 0);
        let mut record = Record::new();

        let add = AddTrackEdit::new(&sequencer, None).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(add));
        assert_eq!(track_colors(&sequencer), vec![0, 4, 1, 2, 3]);
        let remove = RemoveTrackEdit::new(&sequencer, Some(2)).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(remove));
        // The tracks below the removed one keep their colours.
        assert_eq!(track_colors(&sequencer), vec![0, 4, 2, 3]);

        record.undo(&mut sequencer);
        assert_eq!(track_colors(&sequencer), vec![0, 4, 1, 2, 3]);
        record.undo(&mut sequencer);
        record.redo(&mut sequencer);
        assert_eq!(track_colors(&sequencer), vec![0, 4, 1, 2, 3]);
    }

    /// The tracks' names, in order.
    fn names(sequencer: &Sequencer) -> Vec<Option<String>> {
        sequencer
            .tracks()
            .iter()
            .map(|t| t.name().map(str::to_owned))
            .collect()
    }

    fn named(name: &str) -> Option<String> {
        Some(name.to_owned())
    }

    #[test]
    fn rename_names_the_track_and_undo_and_redo_swap_the_names_back() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();

        let rename = RenameTrackEdit::new(&sequencer, 1, named("Bass")).unwrap();
        let result = record.edit(&mut sequencer, SequencerEdit::from(rename));
        assert!(matches!(result, EditResult::TrackRenamed));
        assert_eq!(names(&sequencer), vec![None, named("Bass"), None, None]);
        let rename = RenameTrackEdit::new(&sequencer, 1, None).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(rename));
        assert_eq!(names(&sequencer)[1], None);

        record.undo(&mut sequencer);
        assert_eq!(names(&sequencer)[1], named("Bass"));
        record.undo(&mut sequencer);
        assert_eq!(names(&sequencer)[1], None);
        record.redo(&mut sequencer);
        assert_eq!(names(&sequencer)[1], named("Bass"));
    }

    #[test]
    fn a_rename_that_changes_nothing_is_no_edit() {
        let mut sequencer = test_sequencer();
        assert!(RenameTrackEdit::new(&sequencer, 0, None).is_none());
        sequencer.tracks_mut()[0].set_name(named("Drums"));
        assert!(RenameTrackEdit::new(&sequencer, 0, named("Drums")).is_none());
        assert!(RenameTrackEdit::new(&sequencer, 9, named("Drums")).is_none());
    }

    /// A rename names its track by position; a later remove above it is
    /// undone first, putting the track back where the rename's undo expects.
    #[test]
    fn a_rename_undoes_onto_its_own_track_across_a_remove_above_it() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();
        let rename = RenameTrackEdit::new(&sequencer, 2, named("Keys")).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(rename));
        let remove = RemoveTrackEdit::new(&sequencer, Some(0)).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(remove));
        // The name moves with its track.
        assert_eq!(names(&sequencer), vec![None, named("Keys"), None]);

        record.undo(&mut sequencer);
        record.undo(&mut sequencer);
        assert_eq!(names(&sequencer), vec![None; 4]);
        record.redo(&mut sequencer);
        record.redo(&mut sequencer);
        assert_eq!(names(&sequencer), vec![None, named("Keys"), None]);
    }

    #[test]
    fn add_is_refused_at_the_track_cap() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_count(MAX_TRACKS);
        assert!(AddTrackEdit::new(&sequencer, None).is_none());
        assert!(AddTrackEdit::new(&sequencer, Some(sequencer.tracks().len())).is_none());
    }

    #[test]
    fn remove_is_refused_on_the_last_track_and_with_nothing_selected() {
        let mut sequencer = test_sequencer();
        assert!(RemoveTrackEdit::new(&sequencer, None).is_none());
        sequencer.set_track_count(1);
        select(&mut sequencer, 0);
        assert!(RemoveTrackEdit::new(&sequencer, None).is_none());
        assert!(RemoveTrackEdit::new(&sequencer, Some(0)).is_none());
    }

    #[test]
    fn remove_and_undo_put_the_whole_track_back_in_its_slot() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].add_clip(&clip_at(0, 960));
        sequencer.set_track_volume(1, -6.0);
        sequencer.set_track_pan(1, 0.5);
        sequencer.set_track_muted(1, true);
        sequencer.set_track_volume(2, -12.0);
        select(&mut sequencer, 1);
        let before = ids(&sequencer);
        let mut record = Record::new();

        let edit = RemoveTrackEdit::new(&sequencer, None).unwrap();
        let result = record.edit(&mut sequencer, SequencerEdit::from(edit));

        let EditResult::TrackRemoved {
            track_idx,
            track_id,
            slot,
        } = result
        else {
            panic!("expected TrackRemoved");
        };
        assert_eq!((track_idx, slot), (1, 1));
        assert_eq!(track_id, before[1]);
        assert_eq!(ids(&sequencer), vec![before[0], before[2], before[3]]);
        // Nothing moved in the engine: the track now at 1 still reads its
        // own slot's values, and the freed slot is back at neutral.
        assert_eq!(slots(&sequencer), vec![0, 2, 3]);
        assert_eq!(sequencer.track_volume_db(1), -12.0);
        assert_eq!(sequencer.track_mix.volume_db(1), 0.0);
        assert!(!sequencer.track_mix.muted(1));

        record.undo(&mut sequencer);
        assert_eq!(ids(&sequencer), before);
        assert_eq!(slots(&sequencer), vec![0, 1, 2, 3]);
        assert_eq!(sequencer.tracks()[1].clips().len(), 1);
        assert_eq!(sequencer.track_volume_db(1), -6.0);
        assert_eq!(sequencer.track_pan(1), 0.5);
        assert!(sequencer.track_muted(1));
        assert_eq!(sequencer.track_volume_db(2), -12.0);
    }

    #[test]
    fn an_edit_made_before_a_remove_undoes_on_its_own_track() {
        // Every edit names its track by position: undoing the remove first
        // puts track 1 back, so the earlier carve on track 3 is undone on
        // track 3 again — not on the track that sat there meanwhile.
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[3].add_clip(&clip_at(0, 960));
        let mut record = Record::new();

        let carve = DeleteInRangeEdit::from_track_span(&sequencer, 3, 3, 0, 960).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(carve));
        let remove = RemoveTrackEdit::new(&sequencer, Some(1)).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(remove));
        assert!(sequencer.tracks()[2].clips().is_empty());

        record.undo(&mut sequencer);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), 4);
        assert_eq!(sequencer.tracks()[3].clips().len(), 1);
        assert!(sequencer.tracks()[2].clips().is_empty());
    }

    #[test]
    fn a_restored_plugin_track_names_its_plugin_and_slot_for_the_reload() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_output(2, stub_instrument(vec![1]));
        let track_id = sequencer.track_id_by_index(2).unwrap();
        let mut record = Record::new();

        let remove = RemoveTrackEdit::new(&sequencer, Some(2)).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(remove));

        let Some(EditResult::TrackAdded {
            track_idx: 2,
            track_id: restored_id,
            slot: 2,
            instrument: Some(reload),
        }) = record.undo(&mut sequencer)
        else {
            panic!("expected TrackAdded with its plugin");
        };
        assert_eq!(restored_id, track_id);
        assert_eq!(reload.state, vec![1]);
    }

    #[test]
    fn a_restored_track_whose_slot_was_taken_meanwhile_gets_a_free_one() {
        let mut sequencer = test_sequencer();
        let lifted = sequencer.lift_track(1).unwrap();
        // A new track takes the freed slot 1 (the lowest free).
        let mut add = AddTrackEdit::new(&sequencer, Some(sequencer.tracks().len())).unwrap();
        add.edit(&mut sequencer);
        assert_eq!(sequencer.tracks()[3].slot(), 1);

        sequencer.restore_track(1, lifted).unwrap();
        assert_eq!(slots(&sequencer), vec![0, 4, 2, 3, 1]);
    }

    #[test]
    fn a_removed_midi_tracks_sounding_note_is_released_at_once() {
        let (mut sequencer, midi_out_rx, _plugin_rx) = sounding_note_on(1, None);
        assert_eq!(midi_out(&midi_out_rx), vec![vec![0x91, 60, 100]]);

        let mut remove = RemoveTrackEdit::new(&sequencer, Some(1)).unwrap();
        remove.edit(&mut sequencer);

        // Its tick never runs again, so the note-off can't wait for it.
        assert_eq!(midi_out(&midi_out_rx), vec![vec![0x81, 60, 0]]);
        for _ in 0..480 {
            sequencer.tick(Instant::now());
        }
        assert!(midi_out(&midi_out_rx).is_empty());
    }

    #[test]
    fn a_note_on_a_track_that_moved_up_still_gets_its_note_off() {
        let (mut sequencer, midi_out_rx, _plugin_rx) = sounding_note_on(2, None);
        midi_out(&midi_out_rx);

        let mut remove = RemoveTrackEdit::new(&sequencer, Some(1)).unwrap();
        remove.edit(&mut sequencer);
        for _ in 0..240 {
            sequencer.tick(Instant::now());
        }

        assert_eq!(midi_out(&midi_out_rx), vec![vec![0x82, 60, 0]]);
    }

    #[test]
    fn a_plugin_note_on_a_shifted_track_is_released_on_its_own_slot() {
        let (mut sequencer, _midi_out_rx, mut plugin_rx) =
            sounding_note_on(2, Some(stub_instrument(Vec::new())));
        assert_eq!(plugin_events(&mut plugin_rx), vec![(2, [0x90, 60, 100])]);

        let mut remove = RemoveTrackEdit::new(&sequencer, Some(0)).unwrap();
        remove.edit(&mut sequencer);
        // The track is at position 1 now, but its voice is still slot 2's —
        // a transport stop releases the note there.
        sequencer.release_instrument_notes();

        assert_eq!(plugin_events(&mut plugin_rx), vec![(2, [0x80, 60, 0])]);
    }

    #[test]
    fn a_removed_plugin_tracks_sounding_note_is_released_on_its_slot() {
        let (mut sequencer, _midi_out_rx, mut plugin_rx) =
            sounding_note_on(1, Some(stub_instrument(Vec::new())));
        plugin_events(&mut plugin_rx);

        let mut remove = RemoveTrackEdit::new(&sequencer, Some(1)).unwrap();
        remove.edit(&mut sequencer);

        assert_eq!(plugin_events(&mut plugin_rx), vec![(1, [0x80, 60, 0])]);
        // Slot 1's note record is clear for whichever track takes it next.
        sequencer.release_instrument_notes();
        assert!(plugin_events(&mut plugin_rx).is_empty());
    }
}
