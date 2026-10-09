//! The undo layer: [`SequencerEdit`] (one variant per undoable operation) and
//! [`EditResult`] (what changed, so the handler can fire the right UI events).
//!
//! Every data mutation the user can undo is an `impl undo::Edit` type in
//! `clip_edits/` or `event_edits.rs`, wrapped in a `SequencerEdit` variant by
//! the `sequencer_edit_dispatch!` macro here. `edit()` and `undo()` both return
//! an `EditResult`; `event_handlers/edit_result_handler.rs` turns that into
//! `UiEvent`s. Only the gestures that send an edit per pointer move or tap
//! merge — `DragEventsVelocity`, `ResizeClip`, `DragNotes` and `SetTempo` (one
//! undo step per drag gesture or tap burst, not per mouse-move or tap). Adding an operation: new `Edit` type, new
//! `SequencerEdit` arm, new `EditResult` variant if the UI fan-out differs —
//! see `050-undo-redo.md`.
//!
//! The `EditResult` variants for the compound clip edits share a vocabulary:
//! `updated` = an existing clip whose bounds changed (id kept), `added` = a new
//! clip, `removed` = a deleted clip, `shifted` = a clip moved wholesale along
//! the timeline, `split_*` = the split half of an insert/duplicate,
//! `carve_*` = the range-delete done to clear space for a paste. Each carries
//! `selected_track_idx`/`selected_clip_id` when the operation might have
//! removed the selected clip.

use uuid::Uuid;

use crate::core::input_event::TimeSelectionRect;
use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::{Clip, EventSpaceRetime};
use crate::models::track::InstrumentRef;

use super::Sequencer;

mod clip_edits;
mod event_edits;
mod meter_edit;
mod tempo_edit;
mod track_edits;

pub(crate) use clip_edits::{
    CommitClipEdit, DeleteInRangeEdit, DeleteTimeEdit, DuplicateClipsEdit, DuplicateTimeEdit,
    InsertSilenceEdit, MoveClipEdit, MoveRangeEdit, MuteInRangeEdit, PasteClipsEdit,
    ResizeClipEdit, RetimeClipEdit, SplitClipsEdit,
};
pub(crate) use event_edits::{
    DeleteSelectedEventsEdit, DragEventsVelocityEdit, DragNotesEdit, InsertCaptureEdit,
    InsertNotesEdit, MuteSelectedEventsEdit, NudgeSelectedEventsEdit,
    NudgeSelectedEventsLengthEdit, QuantizeEventsEdit, TransposeSelectedEventsEdit,
};
pub(crate) use meter_edit::SetMeterEdit;
pub(crate) use tempo_edit::{SetTempoEdit, TempoGesture};
pub(crate) use track_edits::{AddTrackEdit, RemoveTrackEdit, RenameTrackEdit};

/// Which clip a paste makes the lead (`EditResult::ClipsPasted`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteLead {
    /// The pasted clip under the cursor on the selected track, if any — the
    /// cursor rule `⌘/Ctrl+V` and the Duplicates share with a split.
    CursorRule,
    /// This clip, wherever the cursor sits (Merge Clips: the merged clip on
    /// the selected track, or the lead it already had).
    Clip(Uuid),
    /// This clip, selected the way a clip band press leaves it: the cursor
    /// on its start, its track selected, its span marqueed (the MIDI clip
    /// import — a dropped clip lands like a dropped band drag).
    ClipSpan {
        /// The clip's track.
        track_idx: usize,
        /// The clip.
        clip_id: Uuid,
    },
}

/// What changed — returned by edit() and undo() so the handler can fire UI
/// events. See the module docs for the shared field vocabulary.
pub(crate) enum EditResult {
    /// Forward direction of a split: `updated` is each shrunk original
    /// (left) clip, `added` is each new remainder (right) clip it produced.
    ClipsSplit {
        /// Existing clips whose bounds changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Newly created clips.
        added: Vec<ClipMetadata>,
    },
    /// Undo of a split: `updated` is each original clip restored to its
    /// pre-split bounds, `removed` is each remainder clip that gets deleted.
    /// Carries selection context (`selected_track_idx`/`selected_clip_id`) in
    /// case the currently-selected clip was one of the removed remainders.
    ClipsUnsplit {
        /// Existing clips whose bounds changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Clips deleted whole.
        removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Forward direction of Insert Silence: `split_updated`/`split_added`
    /// mirror `ClipsSplit` (clips split to make room at the insertion
    /// point), `shifted` is every clip — pre-existing or freshly split —
    /// that moved right by the inserted duration. A clip that was both
    /// split and shifted is reported once, in `split_added`, at its final
    /// (post-shift) position.
    SilenceInserted {
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// The shrunk left halves of the split (ids kept).
        split_updated: Vec<ClipMetadata>,
        /// The new right-hand halves produced by the split.
        split_added: Vec<ClipMetadata>,
    },
    /// Undo of Insert Silence: `shifted` is every clip moved back left,
    /// `split_updated`/`split_removed` mirror `ClipsUnsplit`.
    SilenceRemoved {
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// The shrunk left halves of the split (ids kept).
        split_updated: Vec<ClipMetadata>,
        /// The right-hand split halves, deleted again (undo).
        split_removed: Vec<ClipMetadata>,
    },
    /// Forward direction of "Delete Time" — the opposite of `SilenceInserted`:
    /// `carve_updated`/`carve_added`/`carve_removed` mirror `RangeDeleted`
    /// (the non-rippling carve that clears `[start, end)`), `shifted` is
    /// every clip — pre-existing or freshly carved — that moved left to
    /// close the gap. A clip that was both carved and shifted is reported
    /// once, in `carve_added`, at its final (post-shift) position. Carries
    /// selection context in case the carve removed the selected clip. The
    /// deleted span is gone, so the handler also collapses the Arranger time
    /// selection back to the track cursor — see `delete_time_workflow`.
    TimeDeleted {
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// Existing clips trimmed by the carve (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// New right-hand pieces produced by the carve.
        carve_added: Vec<ClipMetadata>,
        /// Clips cleared whole by the carve.
        carve_removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the carve removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the carve removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Undo of Delete Time: `shifted` is every clip moved back right,
    /// `carve_updated`/`carve_added`/`carve_removed` mirror `RangeRestored`.
    /// `restored_selection` is the original `[start, end)` bounds the
    /// deleted material reappears at, so the handler can put the Arranger
    /// time selection back — the mirror of `TimeDeleted`'s collapse.
    TimeUndeleted {
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// Existing clips restored to their pre-carve bounds (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Clips that reappear whole (undo of the carve's `removed`).
        carve_added: Vec<ClipMetadata>,
        /// The carve's right-hand pieces, deleted again (undo).
        carve_removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
        /// The original time-selection bounds to put back (undo).
        restored_selection: (i32, i32),
    },
    /// Forward direction of Ableton-style range delete: `updated` is each
    /// clip trimmed at one edge (id kept), `added` is each new right-hand
    /// piece produced by a range-spanning clip, `removed` is each clip
    /// cleared whole. Nothing shifts — a gap is left. Carries selection
    /// context (`selected_track_idx`/`selected_clip_id`) in case the selected
    /// clip was cleared. `DeleteInRangeEdit::from_track_span` also produces
    /// this — the plain `⌘/Ctrl+X` marquee-scoped carve is the exact same
    /// edit type, just constructed with a narrower track range.
    RangeDeleted {
        /// Existing clips whose bounds changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Newly created clips.
        added: Vec<ClipMetadata>,
        /// Clips deleted whole.
        removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Undo of a range delete: `added` is each cleared clip reappearing,
    /// `updated` is each trimmed clip restored to full bounds, `removed` is
    /// each right-hand piece being deleted again.
    RangeRestored {
        /// Existing clips whose bounds changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Newly created clips.
        added: Vec<ClipMetadata>,
        /// Clips deleted whole.
        removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Forward direction of "mute the time selection" (`MuteInRangeEdit`):
    /// every clip overlapping the range is split at its edges like
    /// `RangeDeleted`, but nothing is removed — `updated` is each trimmed
    /// original (bounds changed, id kept) plus each pre-existing interior
    /// clip whose mute flag flipped, `added` is each new split-created piece
    /// (already carrying its final mute state).
    RangeMuted {
        /// Existing clips whose bounds and/or mute flag changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Newly created clips (split-off pieces).
        added: Vec<ClipMetadata>,
    },
    /// Undo of "mute the time selection": `updated` is each trimmed clip
    /// restored to full bounds plus each interior clip whose mute flag was
    /// restored (mirroring `RangeMuted`), `removed` is each split-created
    /// piece being deleted again.
    RangeUnmuted {
        /// Existing clips whose bounds and/or mute flag changed (ids kept).
        updated: Vec<ClipMetadata>,
        /// Clips deleted whole (split-off pieces).
        removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Forward direction of a clip paste. `pasted` is every clipboard clip
    /// added at the cursor. The `carve_*` buckets mirror `RangeDeleted` and
    /// come from the per-target `DeleteInRangeEdit`s that clear the span each
    /// pasted clip lands on: `carve_updated` trimmed originals (id kept),
    /// `carve_added` new right-hand pieces, `carve_removed` clips cleared whole.
    ClipsPasted {
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// The clipboard clips added at the cursor.
        pasted: Vec<ClipMetadata>,
        /// Which clip becomes the lead, and how.
        lead: PasteLead,
    },
    /// Undo of a clip paste: `unpasted` is every pasted clip being removed, the
    /// `carve_*` buckets mirror `RangeRestored` (carved originals coming back).
    /// Carries the pre-paste selection so the handler can restore it.
    ClipsUnpasted {
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// The pasted clips being removed again (undo).
        unpasted: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
    },
    /// Forward direction of plain `⌘/Ctrl+D` "Duplicate Clips"
    /// (`DuplicateClipsEdit`): exactly `ClipsPasted`'s buckets — it *is* a
    /// `PasteClipsEdit` anchored at the marquee's `end` — plus
    /// `new_selection`, the marquee slid right by its own width so the view
    /// follows the copy and repeated presses chain down the timeline. Unlike
    /// `TimeDuplicated` the carve buckets are real: the copy overwrites
    /// whatever sat in the destination span.
    ClipsDuplicated {
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// The copied clips added after the marquee.
        pasted: Vec<ClipMetadata>,
        /// Arranger time selection the view should advance to.
        new_selection: TimeSelectionRect,
    },
    /// Undo of "Duplicate Clips": exactly `ClipsUnpasted`'s buckets plus
    /// `restored_selection`, the original marquee rect to put back.
    ClipsUnduplicated {
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// The copied clips being removed again (undo).
        unpasted: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
        /// Arranger time selection the view should restore.
        restored_selection: TimeSelectionRect,
    },
    /// Forward direction of a clip move (`MoveClipEdit`, the arranger band
    /// drag): `from` is the clip as it sat on its source track, `to` the same
    /// clip (same id) at its destination. The `carve_*` buckets mirror
    /// `ClipsPasted`'s — the `DeleteInRangeEdit` that cleared the destination
    /// span. Carries no selection context: the moved clip always survives and
    /// the handler re-selects it (and re-marquees its span) wherever it landed.
    ClipMoved {
        /// The clip at its source position (the shape to remove).
        from: ClipMetadata,
        /// The same clip at its destination (the shape to add).
        to: ClipMetadata,
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
    },
    /// Undo of a clip move: `from` is the clip at the destination it is being
    /// lifted from, `to` the clip back at its original position; the `carve_*`
    /// buckets mirror `RangeRestored` (carved originals coming back).
    ClipUnmoved {
        /// The clip at the destination it is leaving (the shape to remove).
        from: ClipMetadata,
        /// The clip back at its original position (the shape to add).
        to: ClipMetadata,
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
    },
    /// Forward direction of a marquee move (`MoveRangeEdit`, the band drag
    /// started inside an active marquee): `moved_from`/`moved_to` are every
    /// piece as it sat on its source track / at its destination (same ids,
    /// parallel), the `split_*` buckets are the pre-move split of the source
    /// clips (`split_updated` the id-keeping leftovers, `split_added` the new
    /// right-hand leftovers — never a piece), the `carve_*` buckets mirror
    /// `ClipsPasted`'s. A piece split out mid-clip is a fresh id the UI has
    /// never seen, so its `moved_from` remove is a no-op. `new_selection` is
    /// the rect the marquee should follow the content to. No selection
    /// context: the handler re-selects from the moved pieces.
    RangeMoved {
        /// Every piece at its source position (the shapes to remove).
        moved_from: Vec<ClipMetadata>,
        /// The same pieces at their destinations (the shapes to add).
        moved_to: Vec<ClipMetadata>,
        /// Split: id-keeping source clips shrunk to their leftover.
        split_updated: Vec<ClipMetadata>,
        /// Split: new right-hand leftovers.
        split_added: Vec<ClipMetadata>,
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// Arranger time selection the view should move to.
        new_selection: TimeSelectionRect,
    },
    /// Undo of a marquee move: `unmoved` is every piece at the destination it
    /// is being lifted from, `restored` every source clip back at its
    /// pre-move bounds — re-merged, so a shrunk leftover shape may still be
    /// in the UI and each is applied as an add-or-replace — `split_removed`
    /// the right-hand leftovers deleted again by the un-split (never a
    /// piece), the `carve_*` buckets mirror `RangeRestored`.
    /// `restored_selection` is the original rect for the marquee.
    RangeUnmoved {
        /// Every piece at the destination it is leaving (the shapes to remove).
        unmoved: Vec<ClipMetadata>,
        /// Every source clip back at its pre-move bounds (add or replace).
        restored: Vec<ClipMetadata>,
        /// Split: right-hand leftovers deleted again.
        split_removed: Vec<ClipMetadata>,
        /// Range-delete: trimmed originals (ids kept).
        carve_updated: Vec<ClipMetadata>,
        /// Range-delete: new right-hand pieces.
        carve_added: Vec<ClipMetadata>,
        /// Range-delete: clips cleared whole.
        carve_removed: Vec<ClipMetadata>,
        /// Arranger time selection the view should restore.
        restored_selection: TimeSelectionRect,
    },
    /// Forward direction of `Shift+⌘/Ctrl+D` "Duplicate Time" (`DuplicateTimeEdit`):
    /// `shifted`/`split_updated`/`split_added` are the `InsertSilenceEdit` half
    /// (clips pushed right / split to open the gap at the selection end),
    /// `pasted` is the `PasteClipsEdit` half (the copied slice dropped into the
    /// freed span — its carve buckets are always empty because the gap is
    /// pre-cleared). `new_selection` is the range the Arranger time selection
    /// should advance to so repeated `Shift+⌘/Ctrl+D` chains down the timeline.
    TimeDuplicated {
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// The shrunk left halves of the split (ids kept).
        split_updated: Vec<ClipMetadata>,
        /// The new right-hand halves produced by the split.
        split_added: Vec<ClipMetadata>,
        /// The clipboard clips added at the cursor.
        pasted: Vec<ClipMetadata>,
        /// Arranger time selection the view should advance to.
        new_selection: (i32, i32),
    },
    /// Undo of "Duplicate Time": `unpasted` is every copied clip being removed,
    /// `shifted`/`split_updated`/`split_removed` mirror `SilenceRemoved`.
    /// Carries the pre-edit selection context (in case the selected clip was a
    /// copy) and `restored_selection`, the original time selection bounds.
    TimeUnduplicated {
        /// The pasted clips being removed again (undo).
        unpasted: Vec<ClipMetadata>,
        /// Clips moved wholesale along the timeline.
        shifted: Vec<ClipMetadata>,
        /// The shrunk left halves of the split (ids kept).
        split_updated: Vec<ClipMetadata>,
        /// The right-hand split halves, deleted again (undo).
        split_removed: Vec<ClipMetadata>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_track_idx: Option<usize>,
        /// Selection at the time, in case the operation removed the selected clip.
        selected_clip_id: Option<Uuid>,
        /// The original time-selection bounds to put back (undo).
        restored_selection: (i32, i32),
    },
    /// One clip's edges moved (`ResizeClipEdit`, or `RetimeClipEdit`,
    /// which also retimed its events and may have set the tempo; either direction):
    /// its start, its window, or both. Nothing was added or removed, so the
    /// handler updates the shape (bounds and thumbnail, which depends on the
    /// window) and refreshes the open clip view. The transport is left alone
    /// (`220-capture-without-pending-view.md`).
    ClipResized {
        /// The clip at its new bounds.
        clip: ClipMetadata,
        /// How a retime (or its undo) moved the clip's event-tick space,
        /// so the open clip view can follow it; `None` for an edge edit,
        /// which moves no events.
        retime: Option<EventSpaceRetime>,
    },
    /// A brand-new clip is on its track (`CommitClipEdit`, forward direction
    /// — a capture commit, an empty clip (`⇧⌘M`), a redo of either, or of a
    /// live take): add its shape,
    /// select it, re-seek. The live-take workflow drops this result on the
    /// *first* edit since its own fan-out has already run — see `050-undo-redo.md`.
    ClipCommitted {
        /// The clip as placed.
        clip: ClipMetadata,
    },
    /// Undo of a clip commit: the clip has been lifted off its track. No
    /// frozen selection context — lifting doesn't touch the selection, so the
    /// handler reads it live (and leaves the clip view if this clip was open).
    ClipUncommitted {
        /// The clip that was removed.
        clip: ClipMetadata,
    },
    /// Events within one clip changed (nudge / transpose / velocity / delete /
    /// duplicate / a take inserted by `/`) — re-render that clip and re-sync
    /// the event selection.
    EventsModified {
        /// Track the clip is on.
        track_idx: usize,
        /// The clip whose events changed.
        clip_id: Uuid,
        /// The event selection after the edit.
        selected_event_ids: Vec<Uuid>,
        /// Lowest and highest pitch of a take a capture commit just put in
        /// the clip (`InsertCaptureEdit`'s edit and redo; `None` for every
        /// other edit and every undo) — the handler sends
        /// `UiEvent::CaptureInserted` so the piano roll re-frames if the take
        /// landed out of view.
        inserted_take: Option<(u8, u8)>,
    },
    /// A track is in the arrangement at `track_idx` (`AddTrackEdit`'s edit
    /// and redo, `RemoveTrackEdit`'s undo) — a fresh one, or one coming back
    /// whole. Every track from `track_idx` on moved down one; engine state
    /// didn't move (it is keyed by slot). The handler tells the view
    /// (`TracksChanged` with the shift), re-adds its clip shapes, has
    /// `Display` reload a plugin from `instrument` into `slot`, and selects
    /// it.
    TrackAdded {
        /// Where the track now is.
        track_idx: usize,
        /// Its id — what `Display` keeps a departed plugin's live state under.
        track_id: Uuid,
        /// Its engine slot, where `Display` loads its plugin.
        slot: usize,
        /// Its plugin, state blob included — `None` for a MIDI-Out track.
        instrument: Option<InstrumentRef>,
    },
    /// The track at `track_idx` left the arrangement (`RemoveTrackEdit`'s
    /// edit and redo, `AddTrackEdit`'s undo); every later track moved up one.
    /// The handler tells the view (which drops the track's clip shapes with
    /// the shift), has `Display` keep the plugin in `slot`'s live state under
    /// `track_id` and tear it down, and — if it was the selected track —
    /// selects the one that took its place.
    TrackRemoved {
        /// Where the track was.
        track_idx: usize,
        /// Its id.
        track_id: Uuid,
        /// The engine slot it held, where `Display` finds its plugin.
        slot: usize,
    },
    /// A track's name changed (`RenameTrackEdit`'s edit and undo). The
    /// handler re-sends the view its track list (`TracksChanged`, no shift).
    TrackRenamed,
    /// The project tempo changed (`SetTempoEdit`'s edit and undo). The view
    /// reads the tempo from its atomic, so the handler only wakes it.
    TempoChanged,
    /// The project's meter changed (`SetMeterEdit`'s edit and undo). The
    /// view reads the meter from its atomic, so the handler only wakes it.
    MeterChanged,
    /// The edit turned out to be a no-op — nothing for the handler to do.
    NoOp,
}

// ---------------------------------------------------------------------------
// Clip lookup shared by every edit
// ---------------------------------------------------------------------------

impl Sequencer {
    /// The clip with this id on `track_idx`, or `None` if either is gone.
    pub(crate) fn clip_on(&self, track_idx: usize, clip_id: Uuid) -> Option<&Clip> {
        self.tracks().get(track_idx)?.get_clip_by_id(clip_id)
    }

    /// Mutable [`clip_on`](Self::clip_on). Not for moving a clip's events:
    /// that goes through [`edit_clip_events`](Self::edit_clip_events), or a
    /// note sounding under a running playhead can hang.
    pub(crate) fn clip_on_mut(&mut self, track_idx: usize, clip_id: Uuid) -> Option<&mut Clip> {
        self.tracks_mut()
            .get_mut(track_idx)?
            .get_clip_by_id_mut(clip_id)
    }

    /// Runs `edit` on the clip with this id on `track_idx`; `None` if either
    /// is gone. While the transport runs, a note the edit stops sounding at
    /// the playhead is released rather than left hanging
    /// ([`Track::edit_clip_events`](crate::models::track::Track::edit_clip_events)).
    pub(crate) fn edit_clip_events<R>(
        &mut self,
        track_idx: usize,
        clip_id: Uuid,
        edit: impl FnOnce(&mut Clip) -> R,
    ) -> Option<R> {
        if !self.is_running() {
            return self.clip_on_mut(track_idx, clip_id).map(edit);
        }
        self.tracks_mut()
            .get_mut(track_idx)?
            .edit_clip_events(clip_id, edit)
    }
}

// ---------------------------------------------------------------------------
// SequencerEdit enum + undo::Edit dispatch (macro-generated)
// ---------------------------------------------------------------------------

/// Generates the [`SequencerEdit`] enum and its `impl undo::Edit` from a list
/// of `Variant(EditType)` pairs, so a new operation only has to be added to the
/// invocation below. Each edit type also converts `Into` its variant, so a
/// handler can record it without naming the variant. `edit`/`undo` dispatch
/// straight through; `merge` is hand-written to coalesce only
/// `DragEventsVelocity`, `ResizeClip`, `DragNotes` and `SetTempo`.
macro_rules! sequencer_edit_dispatch {
    ($($variant:ident($type:ty)),* $(,)?) => {
        /// Undoable edits on the sequencer.
        pub(crate) enum SequencerEdit {
            $($variant($type)),*
        }

        $(
            impl From<$type> for SequencerEdit {
                fn from(edit: $type) -> Self {
                    SequencerEdit::$variant(edit)
                }
            }
        )*

        impl undo::Edit for SequencerEdit {
            type Target = Sequencer;
            type Output = EditResult;

            fn edit(&mut self, target: &mut Sequencer) -> EditResult {
                match self { $(SequencerEdit::$variant(e) => e.edit(target)),* }
            }

            fn undo(&mut self, target: &mut Sequencer) -> EditResult {
                match self { $(SequencerEdit::$variant(e) => e.undo(target)),* }
            }

            // `DragEventsVelocity`, `ResizeClip`, `DragNotes` and `SetTempo`
            // are hardcoded here rather than expressed generically because
            // they're the only edits that ever want to merge: the ⌘/Ctrl+drag
            // velocity gesture, the clip edge drag, the piano roll's note drag
            // and the BPM chip drag send one edit per `MouseMoved` (tap tempo
            // one per tap), and without merging each of those would become its
            // own undo step.
            // `same_gesture` (drag_id equality) keeps two separate drags from
            // coalescing into one. Every other
            // variant keeps the trait's default (`Merged::No`) — plain
            // discrete edits, one undo step per action, same as today.
            fn merge(&mut self, other: Self) -> undo::Merged<Self> {
                // Each step's nudge is relative, so the merged edit keeps
                // them all for redo to replay.
                if let SequencerEdit::DragEventsVelocity(a) = self
                    && let SequencerEdit::DragEventsVelocity(b) = &other
                    && a.same_gesture(b)
                {
                    a.absorb(b);
                    return undo::Merged::Yes;
                }
                // The clip edge drag, likewise one edit per pointer move.
                // Unlike velocity nudges these are absolute, so the merged
                // edit takes the later step's target bounds.
                if let SequencerEdit::ResizeClip(a) = self
                    && let SequencerEdit::ResizeClip(b) = &other
                    && a.same_gesture(b)
                {
                    a.absorb(b);
                    return undo::Merged::Yes;
                }
                // The note drag: each step holds the whole drag from the
                // press, so the merged edit takes the later one's. A step
                // back to no change leaves the clip as the gesture found it,
                // so the gesture's undo step goes away.
                if let SequencerEdit::DragNotes(a) = self
                    && let SequencerEdit::DragNotes(b) = &other
                    && a.same_gesture(b)
                {
                    if b.is_noop() {
                        return undo::Merged::Annul;
                    }
                    a.absorb(b);
                    return undo::Merged::Yes;
                }
                // A tempo drag or tap burst: absolute steps, like the note
                // drag's, and a step back to the gesture's starting tempo
                // (a drag's Esc) annuls it.
                if let SequencerEdit::SetTempo(a) = self
                    && let SequencerEdit::SetTempo(b) = &other
                    && a.same_gesture(b)
                {
                    if a.is_reverted_by(b) {
                        return undo::Merged::Annul;
                    }
                    a.absorb(b);
                    return undo::Merged::Yes;
                }
                undo::Merged::No(other)
            }
        }
    };
}

sequencer_edit_dispatch!(
    CommitClip(CommitClipEdit),
    DuplicateClips(DuplicateClipsEdit),
    DuplicateTime(DuplicateTimeEdit),
    DeleteInRange(DeleteInRangeEdit),
    DeleteTime(DeleteTimeEdit),
    SplitClips(SplitClipsEdit),
    InsertSilence(InsertSilenceEdit),
    PasteClips(PasteClipsEdit),
    MoveClip(MoveClipEdit),
    MoveRange(MoveRangeEdit),
    ResizeClip(ResizeClipEdit),
    RetimeClip(RetimeClipEdit),
    MuteInRange(MuteInRangeEdit),
    DeleteSelectedEvents(DeleteSelectedEventsEdit),
    NudgeSelectedEvents(NudgeSelectedEventsEdit),
    NudgeSelectedEventsLength(NudgeSelectedEventsLengthEdit),
    TransposeSelectedEvents(TransposeSelectedEventsEdit),
    DragEventsVelocity(DragEventsVelocityEdit),
    MuteSelectedEvents(MuteSelectedEventsEdit),
    QuantizeEvents(QuantizeEventsEdit),
    InsertCapture(InsertCaptureEdit),
    InsertNotes(InsertNotesEdit),
    DragNotes(DragNotesEdit),
    AddTrack(AddTrackEdit),
    RemoveTrack(RemoveTrackEdit),
    RenameTrack(RenameTrackEdit),
    SetTempo(SetTempoEdit),
    SetMeter(SetMeterEdit),
);

#[cfg(test)]
mod tests {

    use undo::Record;

    use crate::models::{clip::Clip, event::Event};

    use crate::core::sequencer::test_support::test_sequencer;

    use super::*;

    /// One selected track/clip with a single NoteOn/NoteOff pair (note 60,
    /// velocity 60). Returns the sequencer plus the NoteOn id.
    fn sequencer_with_one_note() -> (Sequencer, Uuid) {
        let mut sequencer = test_sequencer();
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));

        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(960));
        let clip_id = clip.id();
        clip.add_event(Event::new(0, 0, vec![0x90, 60, 60]));
        clip.add_event(Event::new(100, 0, vec![0x80, 60, 0]));
        let note_id = clip.events()[0].id();

        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.select_clip(Some(clip_id));

        (sequencer, note_id)
    }

    #[test]
    fn drag_events_velocity_same_drag_id_coalesces_into_one_undo_step() {
        let (mut sequencer, note_id) = sequencer_with_one_note();
        let mut record: Record<SequencerEdit> = Record::new();

        let edit_a =
            DragEventsVelocityEdit::from_sequencer(&sequencer, vec![note_id], 5, 42).unwrap();
        let edit_b =
            DragEventsVelocityEdit::from_sequencer(&sequencer, vec![note_id], 3, 42).unwrap();
        record.edit(&mut sequencer, SequencerEdit::DragEventsVelocity(edit_a));
        record.edit(&mut sequencer, SequencerEdit::DragEventsVelocity(edit_b));

        assert_eq!(record.len(), 1, "same drag_id must coalesce to one entry");
        let velocity =
            |seq: &Sequencer| seq.selected_clip().unwrap().events()[0].velocity().unwrap();
        assert_eq!(velocity(&sequencer), 68); // 60 + 5 + 3

        record.undo(&mut sequencer);
        assert_eq!(velocity(&sequencer), 60); // one undo restores the pre-drag state

        // Regression: redo used to replay only the first step (65).
        record.redo(&mut sequencer);
        assert_eq!(velocity(&sequencer), 68);
    }

    #[test]
    fn drag_events_velocity_redo_replays_clamped_steps_in_order() {
        let (mut sequencer, note_id) = sequencer_with_one_note();
        let mut record: Record<SequencerEdit> = Record::new();
        let velocity =
            |seq: &Sequencer| seq.selected_clip().unwrap().events()[0].velocity().unwrap();

        // 60 → up 80 clamps at 127 (+67) → down 20 lands on 107. The steps'
        // sum (+60) would give 120, so redo must replay them in order.
        for nudge in [80, -20] {
            let edit = DragEventsVelocityEdit::from_sequencer(&sequencer, vec![note_id], nudge, 7)
                .unwrap();
            record.edit(&mut sequencer, SequencerEdit::DragEventsVelocity(edit));
        }
        assert_eq!(velocity(&sequencer), 107);

        record.undo(&mut sequencer);
        assert_eq!(velocity(&sequencer), 60);
        record.redo(&mut sequencer);
        assert_eq!(velocity(&sequencer), 107);
    }

    #[test]
    fn drag_events_velocity_different_drag_id_does_not_coalesce() {
        let (mut sequencer, note_id) = sequencer_with_one_note();
        let mut record: Record<SequencerEdit> = Record::new();

        let edit_a =
            DragEventsVelocityEdit::from_sequencer(&sequencer, vec![note_id], 5, 1).unwrap();
        let edit_b =
            DragEventsVelocityEdit::from_sequencer(&sequencer, vec![note_id], 3, 2).unwrap();
        record.edit(&mut sequencer, SequencerEdit::DragEventsVelocity(edit_a));
        record.edit(&mut sequencer, SequencerEdit::DragEventsVelocity(edit_b));

        assert_eq!(record.len(), 2, "different drag_id must not coalesce");
    }
}
