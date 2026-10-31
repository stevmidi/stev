//! Adding, removing, renaming and re-routing tracks: [`AddTrackEdit`] (⌘T,
//! the `+` row), [`RemoveTrackEdit`] (Delete in track header focus, the
//! output menu's `Delete track`), [`RenameTrackEdit`] (⌘R, a double-click on
//! the header's name), [`SetTrackOutputEdit`] (a plugin put on a track,
//! the output menu's channels and `Remove`) and [`SetKnobCcEdit`] (a knob's
//! CC number typed on its label), each one undo step.
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

use uuid::Uuid;

use crate::core::config::{MAX_TRACKS, TRACK_COLOR_COUNT, TRACK_VOLUME_DB_DEFAULT};
use crate::models::knobs::{CcRefusal, cc_refusal};
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
    /// [`TRACK_VOLUME_DB_DEFAULT`], centre, unmuted. Its slot is picked when
    /// it is restored.
    fn fresh(channel: u8, color_slot: usize) -> Self {
        LiftedTrack {
            track: Track::new(TrackOutput::MidiOut { channel }, 0, color_slot),
            volume_db: TRACK_VOLUME_DB_DEFAULT,
            pan: 0.0,
            muted: false,
            soloed: false,
        }
    }
}

impl Sequencer {
    /// Takes the track at `track_idx` out of the arrangement: silences what it
    /// has sounding (while running), puts its slot's mix back at a fresh track's and
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
    /// caught up to the playhead; its mix values and knob positions written
    /// back. Returns where it landed. Never
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
        self.publish_knobs(track_idx);
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
        EditResult::TrackAdded {
            track_idx,
            track_id: track.id(),
            slot: track.slot(),
            instrument: track.output().instrument().cloned(),
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

/// A plugin put on a track from the browser, or the output menu's MIDI
/// channels and `Remove`: routes a track's output somewhere else. `Display`
/// has already swapped the plugin by the time the edit is made; undo and redo
/// have it swap back, from the live state it kept when the plugin went out
/// (`EditResult::TrackOutputChanged`). See `050-undo-redo.md` § Track output.
pub(crate) struct SetTrackOutputEdit {
    /// The track's position.
    track_idx: usize,
    /// The output the track doesn't have: the new one before the edit, the
    /// old one (a plugin with its state blob as of the last save or load)
    /// after it. Edit and undo swap it with the track's, so neither copies
    /// the blob.
    other: TrackOutput,
}

impl SetTrackOutputEdit {
    /// Routes the track at `track_idx` to `output`. `None` with no such
    /// track, or when it is already routed there.
    pub(crate) fn new(
        sequencer: &Sequencer,
        track_idx: usize,
        output: TrackOutput,
    ) -> Option<Self> {
        let current = sequencer.tracks().get(track_idx)?.output();
        (*current != output).then_some(SetTrackOutputEdit {
            track_idx,
            other: output,
        })
    }

    /// Routes the track to its new output.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.swap_track_output(self.track_idx, &mut self.other)
    }

    /// Routes it back.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.swap_track_output(self.track_idx, &mut self.other)
    }
}

impl Sequencer {
    /// Swaps the output of the track at `track_idx` with `output` — both
    /// halves of [`SetTrackOutputEdit`]. While running, what the track has
    /// sounding is released on its old output first, so nothing hangs there.
    fn swap_track_output(&mut self, track_idx: usize, output: &mut TrackOutput) -> EditResult {
        if track_idx >= self.tracks.len() {
            return EditResult::NoOp;
        }
        if self.is_running() {
            self.silence_leaving_track(track_idx);
        }
        let incoming = std::mem::replace(output, TrackOutput::MidiOut { channel: 0 });
        if let Some(old) = self.set_track_output(track_idx, incoming) {
            *output = old;
        }
        let track = &self.tracks[track_idx];
        EditResult::TrackOutputChanged {
            track_id: track.id(),
            slot: track.slot(),
            instrument: track.output().instrument().cloned(),
        }
    }
}

/// A knob's CC number typed on its label (`390-knob-panel.md` decisions 5
/// and 6): the knob sends `to` instead of `from`, and every move of `from`
/// in the track's clips is renumbered with it, so the knob keeps its data
/// and nothing goes on playing out of sight. The moves are named by id,
/// found once when the edit is made, so undo renumbers exactly those back —
/// not moves of `to` that were in the clips already (pasted from another
/// track), which no knob held.
pub(crate) struct SetKnobCcEdit {
    /// The track's position.
    track_idx: usize,
    /// Which knob.
    knob: usize,
    /// Its CC before the edit.
    from: u8,
    /// Its CC after.
    to: u8,
    /// The renumbered moves: per clip id, the event ids.
    moves: Vec<(Uuid, Vec<Uuid>)>,
}

impl SetKnobCcEdit {
    /// Knob `knob` of the track at `track_idx` sends `cc` instead. `Err`
    /// when the knob can't take it (reserved, or another knob's);
    /// `Ok(None)` with no such track or knob, or when it sends `cc` already
    /// — no undo step for a change that changes nothing.
    pub(crate) fn new(
        sequencer: &Sequencer,
        track_idx: usize,
        knob: usize,
        cc: u8,
    ) -> Result<Option<Self>, CcRefusal> {
        let Some(track) = sequencer.tracks().get(track_idx) else {
            return Ok(None);
        };
        let Some(from) = track.knobs().get(knob).map(|knob| knob.cc) else {
            return Ok(None);
        };
        if let Some(refusal) = cc_refusal(track.knobs(), knob, cc) {
            return Err(refusal);
        }
        if from == cc {
            return Ok(None);
        }
        let moves = track
            .clips()
            .iter()
            .map(|clip| (clip.id(), clip.controller_move_ids(from)))
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        Ok(Some(SetKnobCcEdit {
            track_idx,
            knob,
            from,
            to: cc,
            moves,
        }))
    }

    /// Gives the knob its new CC, its moves with it.
    pub(crate) fn edit(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_knob_cc(self.track_idx, self.knob, self.to, &self.moves)
    }

    /// Gives it back its old one.
    pub(crate) fn undo(&mut self, sequencer: &mut Sequencer) -> EditResult {
        sequencer.set_knob_cc(self.track_idx, self.knob, self.from, &self.moves)
    }
}

impl Sequencer {
    /// Puts knob `knob` of the track at `track_idx` on `cc` and renumbers
    /// `moves` to it — both halves of [`SetKnobCcEdit`]. Only the controller
    /// number changes, never a tick, so playback needs no re-seek.
    fn set_knob_cc(
        &mut self,
        track_idx: usize,
        knob: usize,
        cc: u8,
        moves: &[(Uuid, Vec<Uuid>)],
    ) -> EditResult {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return EditResult::NoOp;
        };
        let Some(slot) = track.knob_mut(knob) else {
            return EditResult::NoOp;
        };
        slot.cc = cc;
        for (clip_id, ids) in moves {
            if let Some(clip) = track.get_clip_by_id_mut(*clip_id) {
                clip.renumber_controller_moves(ids, cc);
            }
        }
        EditResult::KnobsChanged
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

    use crate::core::config::{MAX_TRACKS, TRACK_COLOR_COUNT, TRACK_VOLUME_DB_DEFAULT};
    use crate::core::midi::out_queue::MidiOutMessage;
    use crate::core::sequencer::test_support::{
        clip_at, note_off, note_on, sequencer_with_outputs, stub_instrument, test_sequencer,
        track_colors,
    };
    use crate::core::sequencer::{
        ClipInstrumentEvent, DeleteInRangeEdit, EditResult, Sequencer, SequencerEdit,
    };
    use crate::models::event::Event;
    use crate::models::knobs::CcRefusal;
    use crate::models::track::TrackOutput;

    use super::{
        AddTrackEdit, RemoveTrackEdit, RenameTrackEdit, SetKnobCcEdit, SetTrackOutputEdit,
        next_color_slot,
    };

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
    fn an_added_track_starts_at_the_default_volume_whatever_its_slot_held() {
        let mut sequencer = test_sequencer();
        // The idle slot the new track will take (slot 4), left loud.
        sequencer.track_mix.volume_db[4].store(3.0f32.to_bits(), Ordering::Relaxed);
        let mut record = Record::new();

        let edit = AddTrackEdit::new(&sequencer, None).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(edit));

        assert_eq!(sequencer.tracks()[4].slot(), 4);
        assert_eq!(sequencer.track_volume_db(4), TRACK_VOLUME_DB_DEFAULT);
        assert_eq!(sequencer.track_pan(4), 0.0);
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
        sequencer.set_track_volume(2, -3.0);
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
        // own slot's values, and the freed slot is back at a fresh track's mix.
        assert_eq!(slots(&sequencer), vec![0, 2, 3]);
        assert_eq!(sequencer.track_volume_db(1), -3.0);
        assert_eq!(sequencer.track_mix.volume_db(1), TRACK_VOLUME_DB_DEFAULT);
        assert!(!sequencer.track_mix.muted(1));

        record.undo(&mut sequencer);
        assert_eq!(ids(&sequencer), before);
        assert_eq!(slots(&sequencer), vec![0, 1, 2, 3]);
        assert_eq!(sequencer.tracks()[1].clips().len(), 1);
        assert_eq!(sequencer.track_volume_db(1), -6.0);
        assert_eq!(sequencer.track_pan(1), 0.5);
        assert!(sequencer.track_muted(1));
        assert_eq!(sequencer.track_volume_db(2), -3.0);
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

    /// Track `track_idx`'s output.
    fn output(sequencer: &Sequencer, track_idx: usize) -> TrackOutput {
        sequencer.tracks()[track_idx].output().clone()
    }

    #[test]
    fn set_output_routes_the_track_and_undo_and_redo_route_it_back() {
        let mut sequencer = test_sequencer();
        let mut record = Record::new();
        let before = output(&sequencer, 1);
        let synth = stub_instrument(vec![7]);
        let slot = sequencer.tracks()[1].slot();
        let track_id = sequencer.tracks()[1].id();

        let edit = SetTrackOutputEdit::new(&sequencer, 1, synth.clone()).unwrap();
        let result = record.edit(&mut sequencer, SequencerEdit::from(edit));
        assert_eq!(output(&sequencer, 1), synth);
        assert!(matches!(
            result,
            EditResult::TrackOutputChanged { track_id: id, slot: s, instrument: Some(ref i) }
                if id == track_id && s == slot && i.state == vec![7]
        ));

        let result = record.undo(&mut sequencer).unwrap();
        assert_eq!(output(&sequencer, 1), before);
        assert!(matches!(
            result,
            EditResult::TrackOutputChanged {
                instrument: None,
                ..
            }
        ));
        record.redo(&mut sequencer);
        assert_eq!(output(&sequencer, 1), synth);
    }

    #[test]
    fn an_output_change_that_changes_nothing_is_no_edit() {
        let sequencer = test_sequencer();
        let current = output(&sequencer, 0);
        assert!(SetTrackOutputEdit::new(&sequencer, 0, current).is_none());
        let synth = stub_instrument(Vec::new());
        assert!(SetTrackOutputEdit::new(&sequencer, 9, synth).is_none());
    }

    #[test]
    fn an_output_change_and_its_undo_keep_the_tracks_volume_and_pan() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_volume(1, -3.5);
        sequencer.set_track_pan(1, 0.25);
        let mut record = Record::new();

        let edit = SetTrackOutputEdit::new(&sequencer, 1, stub_instrument(Vec::new())).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(edit));
        assert_eq!(sequencer.track_volume_db(1), -3.5);
        assert_eq!(sequencer.track_pan(1), 0.25);

        record.undo(&mut sequencer);
        assert_eq!(sequencer.track_volume_db(1), -3.5);
        assert_eq!(sequencer.track_pan(1), 0.25);
    }

    /// While running, a note sounding on the old output is released there:
    /// the new output never saw its note-on.
    #[test]
    fn an_output_change_releases_the_notes_sounding_on_the_old_output() {
        let (mut sequencer, midi_out_rx, mut plugin_rx) = sounding_note_on(1, None);
        assert_eq!(midi_out(&midi_out_rx), vec![vec![0x91, 60, 100]]);

        let synth = stub_instrument(Vec::new());
        let mut edit = SetTrackOutputEdit::new(&sequencer, 1, synth).unwrap();
        edit.edit(&mut sequencer);

        assert_eq!(midi_out(&midi_out_rx), vec![vec![0x81, 60, 0]]);
        for _ in 0..480 {
            sequencer.tick(Instant::now());
        }
        // The clip's own note-off plays into the plugin, harmlessly; no
        // note-on reaches it.
        assert_eq!(plugin_events(&mut plugin_rx), vec![(1, [0x80, 60, 0])]);
    }

    /// A move of controller `cc` to `value` at `tick`.
    fn cc_at(tick: i32, cc: u8, value: u8) -> Event {
        Event::new(tick, 0, vec![0xB0, cc, value])
    }

    /// Every message in the clips of the track at `track_idx`, clip by clip.
    fn messages(sequencer: &Sequencer, track_idx: usize) -> Vec<Vec<u8>> {
        sequencer.tracks()[track_idx]
            .clips()
            .iter()
            .flat_map(|clip| clip.events().iter().map(|e| e.midi_message().to_vec()))
            .collect()
    }

    /// Track 0 with two clips holding moves of CC 14 (knob 1's), CC 15
    /// (knob 2's), CC 30 (no knob's) and a note; track 1 with a CC 14 move
    /// of its own.
    fn sequencer_with_knob_moves() -> Sequencer {
        let mut sequencer = test_sequencer();
        let mut first = clip_at(0, 960);
        first.add_event(cc_at(0, 14, 10));
        first.add_event(note_on(10));
        first.add_event(cc_at(20, 15, 20));
        first.add_event(note_off(240));
        let mut second = clip_at(960, 960);
        second.add_event(cc_at(0, 30, 40));
        second.add_event(cc_at(100, 14, 50));
        sequencer.tracks_mut()[0].add_clip(&first);
        sequencer.tracks_mut()[0].add_clip(&second);
        let mut other = clip_at(0, 960);
        other.add_event(cc_at(0, 14, 60));
        sequencer.tracks_mut()[1].add_clip(&other);
        sequencer
    }

    #[test]
    fn a_knobs_new_cc_renumbers_its_moves_on_its_track_and_nothing_else() {
        let mut sequencer = sequencer_with_knob_moves();
        let mut record = Record::new();

        let edit = SetKnobCcEdit::new(&sequencer, 0, 0, 74).unwrap().unwrap();
        let result = record.edit(&mut sequencer, SequencerEdit::from(edit));

        assert!(matches!(result, EditResult::KnobsChanged));
        assert_eq!(sequencer.tracks()[0].knobs()[0].cc, 74);
        assert_eq!(
            messages(&sequencer, 0),
            vec![
                vec![0xB0, 74, 10],
                vec![0x90, 60, 100],
                vec![0xB0, 15, 20],
                vec![0x80, 60, 0],
                vec![0xB0, 30, 40],
                vec![0xB0, 74, 50],
            ]
        );
        // Another track's knob 1 is its own.
        assert_eq!(messages(&sequencer, 1), vec![vec![0xB0, 14, 60]]);
        assert_eq!(sequencer.tracks()[1].knobs()[0].cc, 14);
    }

    #[test]
    fn undo_and_redo_give_back_the_number_and_the_moves_it_renumbered() {
        let mut sequencer = sequencer_with_knob_moves();
        let before = messages(&sequencer, 0);
        let mut record = Record::new();

        // Onto CC 30, which a move no knob held already uses: undo must put
        // back only the moves the edit renumbered.
        let edit = SetKnobCcEdit::new(&sequencer, 0, 0, 30).unwrap().unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(edit));
        let after = messages(&sequencer, 0);
        assert_eq!(after[0], vec![0xB0, 30, 10]);

        record.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].knobs()[0].cc, 14);
        assert_eq!(messages(&sequencer, 0), before);

        record.redo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].knobs()[0].cc, 30);
        assert_eq!(messages(&sequencer, 0), after);
    }

    #[test]
    fn a_cc_another_knob_sends_or_no_knob_may_send_is_refused() {
        let sequencer = sequencer_with_knob_moves();
        assert!(matches!(
            SetKnobCcEdit::new(&sequencer, 0, 0, 15),
            Err(CcRefusal::Taken(1))
        ));
        assert!(matches!(
            SetKnobCcEdit::new(&sequencer, 0, 0, 1),
            Err(CcRefusal::Reserved)
        ));
        assert!(matches!(
            SetKnobCcEdit::new(&sequencer, 0, 0, 123),
            Err(CcRefusal::Reserved)
        ));
    }

    #[test]
    fn a_knob_cc_change_that_changes_nothing_is_no_edit() {
        let sequencer = sequencer_with_knob_moves();
        assert!(matches!(SetKnobCcEdit::new(&sequencer, 0, 0, 14), Ok(None)));
        assert!(matches!(SetKnobCcEdit::new(&sequencer, 0, 8, 40), Ok(None)));
        assert!(matches!(
            SetKnobCcEdit::new(&sequencer, 99, 0, 40),
            Ok(None)
        ));
    }

    #[test]
    fn a_turned_knob_is_published_and_comes_back_with_its_removed_track() {
        let mut sequencer = test_sequencer();
        sequencer.set_knob_value(1, 3, 99);
        assert_eq!(sequencer.tracks()[1].knobs()[3].value, Some(99));
        assert_eq!(sequencer.track_mix.knob_value(1, 3), Some(99));
        // Clamped to a CC value.
        sequencer.set_knob_value(1, 4, 200);
        assert_eq!(sequencer.track_mix.knob_value(1, 4), Some(127));

        let mut record = Record::new();
        let edit = RemoveTrackEdit::new(&sequencer, Some(1)).unwrap();
        record.edit(&mut sequencer, SequencerEdit::from(edit));
        // The freed slot's knobs read unturned.
        assert_eq!(sequencer.track_mix.knob_value(1, 3), None);

        record.undo(&mut sequencer);
        assert_eq!(sequencer.track_mix.knob_value(1, 3), Some(99));
    }

    #[test]
    fn a_load_sends_each_tracks_turned_knobs_to_its_own_output() {
        let (mut sequencer, midi_out_rx, mut plugin_rx) = sequencer_with_outputs(false);
        sequencer.tracks_mut()[2].set_output(stub_instrument(Vec::new()));
        sequencer.set_knob_value(1, 0, 64);
        sequencer.set_knob_value(2, 7, 5);

        sequencer.send_knob_positions();

        // Track 2 on MIDI channel 2; untouched knobs send nothing.
        assert_eq!(midi_out(&midi_out_rx), vec![vec![0xB1, 14, 64]]);
        assert_eq!(plugin_events(&mut plugin_rx), vec![(2, [0xB0, 21, 5])]);
    }
}
