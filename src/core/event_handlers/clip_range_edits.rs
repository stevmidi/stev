//! `EventHandlers` workflows for the Arranger range operations — cut, and the
//! `time-bounds`-vs-selected-clip precedence for the carve/paste family. The
//! operand is resolved in the view; the precedence rule lives here
//! (`080-conventions.md`). See `020-views-and-state.md`, `050-undo-redo.md`.
//!
//! None of these workflows release notes themselves: every edit below queues
//! its own affected track's open notes for release, scoped to exactly the
//! clip(s) it touches, before mutating anything (`Track::release_sounding_notes_for_clip`,
//! `050-undo-redo.md`). A project-wide `NoteLogger` `ReleaseNotes` used to run
//! here instead, which silenced every sounding note on every track for a
//! single-track edit — see `050-undo-redo.md`.

use std::slice;

use undo::Record;

use crate::core::input_event::{CLIP_CLIPBOARD_SENTINEL, TimeSelectionRect};
use crate::core::sequencer::{DeleteInRangeEdit, PasteLead, SequencerEdit};

use super::*;

/// The three UI buckets a range carve produces — trimmed originals (id kept),
/// new right-hand pieces, and clips cleared whole. Groups the `carve_*` fields
/// of `EditResult::ClipsPasted`/`ClipsUnpasted` so the paste workflows stay
/// under the argument-count lint.
pub(super) struct CarveResult<'a> {
    /// Trimmed originals (ids kept).
    pub updated: &'a [ClipMetadata],
    /// New right-hand pieces.
    pub added: &'a [ClipMetadata],
    /// Clips cleared whole.
    pub removed: &'a [ClipMetadata],
}

impl<'a> CarveResult<'a> {
    /// The buckets, in field order.
    pub(super) fn new(
        updated: &'a [ClipMetadata],
        added: &'a [ClipMetadata],
        removed: &'a [ClipMetadata],
    ) -> Self {
        CarveResult {
            updated,
            added,
            removed,
        }
    }
}

impl CarveResult<'static> {
    /// No carve: the edit's gap was cleared beforehand.
    pub(super) const EMPTY: Self = CarveResult {
        updated: &[],
        added: &[],
        removed: &[],
    };
}

/// The clip under the cursor on the selected track, if it is one of
/// `candidates` — the "the clip at the cursor becomes the selection" rule a
/// split, a paste and a range move share.
fn cursor_clip_among(sequencer: &Sequencer, candidates: &[ClipMetadata]) -> Option<Uuid> {
    sequencer
        .find_selected_track_clip_id_at_cursor()
        .filter(|id| candidates.iter().any(|m| m.clip_id == *id))
}

impl EventHandlers {
    /// Sends a forward carve's buckets: trimmed originals, new pieces, then
    /// clips cleared whole.
    fn send_carve_ui_events(&self, carve: &CarveResult) {
        self.send_clips_updated_ui_event(carve.updated);
        self.send_clips_added_ui_event(carve.added);
        self.send_clips_removed_ui_event(carve.removed);
    }

    /// Sends an undone carve's buckets: cleared clips reappearing first, then
    /// the restored originals, then the pieces deleted again.
    fn send_uncarve_ui_events(&self, carve: &CarveResult) {
        self.send_clips_added_ui_event(carve.added);
        self.send_clips_updated_ui_event(carve.updated);
        self.send_clips_removed_ui_event(carve.removed);
    }

    /// Reselects the clip that was selected before the edit if it is on its
    /// track, otherwise clears the selection — the undo workflows' fallback.
    fn reselect_surviving_clip(
        &self,
        sequencer: &mut Sequencer,
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        let survivor = selected_track_idx
            .zip(selected_clip_id)
            .filter(|&(track_idx, clip_id)| sequencer.clip_on(track_idx, clip_id).is_some())
            .map(|(_, clip_id)| clip_id);
        self.select_clip_workflow(sequencer, survivor);
    }

    /// Forward direction of a split: `updated` is each shrunk original
    /// (left) clip, `added` is each new remainder (right) clip. The cursor
    /// sits exactly on the boundary it split at, which is also where a
    /// user's attention naturally moves next — like Ableton, the right-hand
    /// piece on the current track becomes selected (same cursor-clip
    /// reselection rule as `paste_clips_workflow`).
    pub(super) fn split_clips_workflow(
        &self,
        sequencer: &mut Sequencer,
        updated: &[ClipMetadata],
        added: &[ClipMetadata],
    ) {
        sequencer.reset();

        self.send_clips_updated_ui_event(updated);
        self.send_clips_added_ui_event(added);

        if let Some(cursor_clip_id) = cursor_clip_among(sequencer, added) {
            self.select_clip_workflow(sequencer, Some(cursor_clip_id));
        }
    }

    /// Undo of a split: `updated` is each original clip restored to its
    /// pre-split bounds, `removed` is each right-hand piece being deleted.
    /// Since a split leaves its right-hand piece selected (see
    /// `split_clips_workflow`), the common case here is that
    /// `selected_clip_id` names one of `removed` — fall back to the
    /// original (now-restored) clip it was split from rather than
    /// `delete_in_range_workflow`'s usual "clear the selection" fallback for
    /// a removed clip with nothing to replace it.
    pub(super) fn unsplit_clips_workflow(
        &self,
        sequencer: &mut Sequencer,
        updated: &[ClipMetadata],
        removed: &[ClipMetadata],
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        let restored_original_id =
            selected_track_idx
                .zip(selected_clip_id)
                .and_then(|(track_idx, clip_id)| {
                    removed
                        .iter()
                        .position(|clip| clip.track_idx == track_idx && clip.clip_id == clip_id)
                        .and_then(|i| updated.get(i))
                        .map(|clip| clip.clip_id)
                });

        match restored_original_id {
            Some(original_id) => self.select_clip_workflow(sequencer, Some(original_id)),
            None => self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id),
        }

        self.send_clips_removed_ui_event(removed);
        self.send_clips_updated_ui_event(updated);
    }

    /// Forward direction of Insert Silence: `split_updated`/`split_added`
    /// mirror `split_clips_workflow`'s handling of a split performed to make
    /// room at the insertion point; `shifted` is every clip that moved right
    /// by the inserted duration. Unlike `split_clips_workflow`, there is no
    /// cursor-follow reselection here — a shifted or split-left clip keeps
    /// its id, so whatever was already selected stays valid and simply
    /// tracks its new position via the `ClipUpdated` events below.
    pub(super) fn insert_silence_workflow(
        &self,
        sequencer: &mut Sequencer,
        shifted: &[ClipMetadata],
        split_updated: &[ClipMetadata],
        split_added: &[ClipMetadata],
    ) {
        sequencer.reset();

        self.send_clips_updated_ui_event(split_updated);
        self.send_clips_added_ui_event(split_added);
        self.send_clips_updated_ui_event(shifted);
    }

    /// Undo of Insert Silence: `shifted` is every clip moved back left,
    /// `split_updated`/`split_removed` mirror `unsplit_clips_workflow`. No
    /// reselection here either, for the same reason as
    /// `insert_silence_workflow` above.
    pub(super) fn remove_silence_workflow(
        &self,
        sequencer: &mut Sequencer,
        shifted: &[ClipMetadata],
        split_updated: &[ClipMetadata],
        split_removed: &[ClipMetadata],
    ) {
        sequencer.reset();

        self.send_clips_removed_ui_event(split_removed);
        self.send_clips_updated_ui_event(split_updated);
        self.send_clips_updated_ui_event(shifted);
    }

    /// Forward direction of Ableton-style range delete: `carve.updated` is
    /// each clip trimmed at one edge (id kept), `carve.added` is each new
    /// right-hand piece a range-spanning clip produced, `carve.removed` is
    /// each clip cleared whole. Nothing shifts — a gap is left. Selection
    /// sticks to the pre-delete clip if it survived, otherwise clears. Every
    /// removal now flows through here (or a workflow composing the same
    /// carve) — there is no single-clip removal edit any more.
    pub(super) fn delete_in_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        carve: CarveResult,
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
        self.send_carve_ui_events(&carve);
    }

    /// Forward direction of "Delete Time" — the opposite of
    /// `insert_silence_workflow`: `carve` mirrors `delete_in_range_workflow`
    /// (the non-rippling carve that clears `[start, end)`), `shifted` is every
    /// clip — pre-existing or freshly carved — that moved left to close the
    /// gap. Selection sticks to the pre-delete clip if it survived (in place
    /// or shifted), otherwise clears, like `delete_in_range_workflow`.
    pub(super) fn delete_time_workflow(
        &self,
        sequencer: &mut Sequencer,
        shifted: &[ClipMetadata],
        carve: CarveResult,
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
        self.send_carve_ui_events(&carve);
        self.send_clips_updated_ui_event(shifted);
    }

    /// Undo of Delete Time: `shifted` is every clip moved back right, `carve`
    /// mirrors `restore_range_workflow` (the carve unwinding — cleared
    /// material reappearing, trimmed clips restored, the carve's boundary
    /// piece deleted again). Reselects the clip that was selected before the
    /// delete if it's back.
    pub(super) fn restore_time_workflow(
        &self,
        sequencer: &mut Sequencer,
        shifted: &[ClipMetadata],
        carve: CarveResult,
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.send_clips_updated_ui_event(shifted);
        self.send_uncarve_ui_events(&carve);

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
    }

    /// Forward direction of "mute the time selection" (`M`): `updated` is
    /// each clip that changed in place (a split-trimmed original, id kept, or
    /// a pre-existing interior clip whose mute flag flipped), `added` is each
    /// new split-created piece (already carrying its final mute state).
    /// Nothing is removed and the selection is untouched — every affected
    /// clip keeps its id (or, for new pieces, is simply not the selection).
    pub(super) fn mute_in_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        updated: &[ClipMetadata],
        added: &[ClipMetadata],
    ) {
        sequencer.reset();

        self.send_clips_updated_ui_event(updated);
        self.send_clips_added_ui_event(added);
    }

    /// Undo of "mute the time selection": `updated` is each trimmed clip
    /// restored to full bounds (mute flag already restored beforehand),
    /// `removed` is each split-created piece being deleted again. Reselects
    /// the clip that was selected before the edit if it survived, otherwise
    /// clears — same fallback as `restore_range_workflow`.
    pub(super) fn unmute_in_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        updated: &[ClipMetadata],
        removed: &[ClipMetadata],
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.send_clips_updated_ui_event(updated);
        self.send_clips_removed_ui_event(removed);

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
    }

    /// Forward direction of a clip paste (`⌘V`): `pasted` is every clip added
    /// at the paste anchor; `carve` mirrors `delete_in_range_workflow`
    /// (existing material cleared to make room). `lead` picks the selection:
    /// the pasted clip under the cursor on the selected track (same
    /// cursor-clip reselection rule as `split_clips_workflow`; if none, the
    /// selection clears), a clip the edit names (Merge Clips, wherever the
    /// cursor sits), or a named clip selected as its span (the MIDI clip
    /// import, landing like a dropped band drag — `select_clip_span_workflow`).
    pub(super) fn paste_clips_workflow(
        &self,
        sequencer: &mut Sequencer,
        carve: CarveResult,
        pasted: &[ClipMetadata],
        lead: PasteLead,
    ) {
        sequencer.reset();

        self.send_carve_ui_events(&carve);
        self.send_clips_added_ui_event(pasted);

        match lead {
            PasteLead::CursorRule => {
                let clip_to_select = cursor_clip_among(sequencer, pasted);
                self.select_clip_workflow(sequencer, clip_to_select);
            }
            PasteLead::Clip(clip_id) => self.select_clip_workflow(sequencer, Some(clip_id)),
            PasteLead::ClipSpan { track_idx, clip_id } => {
                self.select_clip_span_workflow(sequencer, track_idx, clip_id);
            }
        }
    }

    /// Undo of a clip paste: `unpasted` is every pasted clip being removed;
    /// `carve` mirrors `restore_range_workflow` (carved material coming back).
    /// Restores the pre-paste selection if that clip is still around.
    pub(super) fn unpaste_clips_workflow(
        &self,
        sequencer: &mut Sequencer,
        carve: CarveResult,
        unpasted: &[ClipMetadata],
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.send_clips_removed_ui_event(unpasted);
        self.send_uncarve_ui_events(&carve);

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
    }

    /// Forward direction of a clip move (the arranger band drag): `from` is
    /// the clip where it was lifted from, `to` the same clip (same id) at its
    /// destination; `carve` mirrors `paste_clips_workflow`'s buckets. Always a
    /// remove + add, even on the same track — clip shapes are keyed by
    /// `(track_idx, clip_id)` and `update_clip_shape` can't change lanes. The
    /// clip selection is cleared *first* so the reselect inside
    /// `select_clip_span_workflow` is a real state change on an unchanged id
    /// (model-only — nothing renders the lead clip). That final step leaves
    /// the clip exactly as a fresh band press would: cursor on its start,
    /// selected, its span marqueed.
    pub(super) fn move_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        from: &ClipMetadata,
        to: &ClipMetadata,
        carve: CarveResult,
    ) {
        sequencer.reset();
        self.clear_clip_selection_workflow(sequencer);

        self.send_clips_removed_ui_event(slice::from_ref(from));
        self.send_carve_ui_events(&carve);
        self.send_clips_added_ui_event(slice::from_ref(to));

        self.select_clip_span_workflow(sequencer, to.track_idx, to.clip_id);
    }

    /// Undo of a clip move: `from` is the clip at the destination it is being
    /// lifted from, `to` the clip back at its original position; `carve`
    /// mirrors `unpaste_clips_workflow`'s buckets (carved originals coming
    /// back). Same remove + add + span-reselect shape as
    /// `move_clip_workflow`, on the restored position.
    pub(super) fn unmove_clip_workflow(
        &self,
        sequencer: &mut Sequencer,
        from: &ClipMetadata,
        to: &ClipMetadata,
        carve: CarveResult,
    ) {
        sequencer.reset();
        self.clear_clip_selection_workflow(sequencer);

        self.send_clips_removed_ui_event(slice::from_ref(from));
        self.send_uncarve_ui_events(&carve);
        self.send_clips_added_ui_event(slice::from_ref(to));

        self.select_clip_span_workflow(sequencer, to.track_idx, to.clip_id);
    }

    /// Forward direction of a marquee move (the band drag started inside an
    /// active marquee): `moved_from`/`moved_to` are every piece at its
    /// source / destination, `split_updated`/`split_added` the source clips'
    /// pre-move split (sent like `split_clips_workflow` does, before the
    /// carve), `carve` mirrors `paste_clips_workflow`'s buckets. Same
    /// clear-first + remove + add shape as `move_clip_workflow`, for N
    /// pieces, then `select_range_workflow` puts the marquee on
    /// `new_selection` so the block stays selected where it landed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn move_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        moved_from: &[ClipMetadata],
        moved_to: &[ClipMetadata],
        split_updated: &[ClipMetadata],
        split_added: &[ClipMetadata],
        carve: CarveResult,
        new_selection: TimeSelectionRect,
    ) {
        sequencer.reset();
        self.clear_clip_selection_workflow(sequencer);

        self.send_clips_removed_ui_event(moved_from);
        self.send_clips_updated_ui_event(split_updated);
        self.send_clips_added_ui_event(split_added);
        self.send_carve_ui_events(&carve);
        self.send_clips_added_ui_event(moved_to);

        self.select_range_workflow(sequencer, new_selection, moved_to);
    }

    /// Undo of a marquee move: `unmoved` is every piece at the destination
    /// it is leaving, `restored` every source clip back at its pre-move
    /// bounds (`ClipAdded`'s add-or-replace covers a leftover shape the UI
    /// still has and one that left with its piece alike), `split_removed`
    /// the right-hand leftovers deleted again, `carve` mirrors
    /// `unpaste_clips_workflow`'s buckets. Then the marquee goes back on
    /// `restored_selection`.
    pub(super) fn unmove_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        unmoved: &[ClipMetadata],
        restored: &[ClipMetadata],
        split_removed: &[ClipMetadata],
        carve: CarveResult,
        restored_selection: TimeSelectionRect,
    ) {
        sequencer.reset();
        self.clear_clip_selection_workflow(sequencer);

        self.send_clips_removed_ui_event(unmoved);
        self.send_uncarve_ui_events(&carve);
        self.send_clips_removed_ui_event(split_removed);
        self.send_clips_added_ui_event(restored);

        self.select_range_workflow(sequencer, restored_selection, restored);
    }

    /// The marquee-block equivalent of `select_clip_span_workflow`: cursor
    /// to `rect.start`, `rect.track_start` selected, the clip under the
    /// cursor there selected if it is one of `candidates` (the moved pieces
    /// / the restored originals — same "the clip at the cursor becomes the
    /// selection" rule as `paste_clips_workflow`), then the marquee set to
    /// `rect` — sent last so the `TimeSelectionSet` handler re-latches
    /// the cursor and track it just moved (`020-views-and-state.md`).
    fn select_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        rect: TimeSelectionRect,
        candidates: &[ClipMetadata],
    ) {
        sequencer.set_cursor_tick(rect.start);
        self.select_track_workflow(sequencer, rect.track_start);
        let clip_to_select = cursor_clip_among(sequencer, candidates);
        self.select_clip_workflow(sequencer, clip_to_select);

        self.send_time_selection_set_ui_event(
            rect.start,
            rect.end,
            Some((rect.track_start, rect.track_end)),
        );
    }

    /// Undo of a range delete: `carve.added` is each cleared clip
    /// reappearing, `carve.updated` is each trimmed clip restored to full
    /// bounds, `carve.removed` is each right-hand piece being deleted again.
    /// Reselects the clip that was selected before the delete if it is back.
    pub(super) fn restore_range_workflow(
        &self,
        sequencer: &mut Sequencer,
        carve: CarveResult,
        selected_track_idx: Option<usize>,
        selected_clip_id: Option<Uuid>,
    ) {
        sequencer.reset();

        self.send_uncarve_ui_events(&carve);

        self.reselect_surviving_clip(sequencer, selected_track_idx, selected_clip_id);
    }

    /// Carves the marquee rectangle out of the marqueed tracks
    /// (`DeleteInRangeEdit::from_track_span`), undoably — `Delete`/`Backspace`,
    /// and the delete half of `cut_clips_scoped_workflow`. Marquee-only: there
    /// is no selected-clip fallback.
    pub(super) fn delete_rect_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        rect: TimeSelectionRect,
    ) {
        let edit = DeleteInRangeEdit::from_track_span(
            sequencer,
            rect.track_start,
            rect.track_end,
            rect.start,
            rect.end,
        );
        self.record_edit(sequencer, undo_record, edit);
    }

    /// Cut (`Shift+⌘/Ctrl+X`): fill the session clipboard exactly like
    /// `CopyClips` (non-undoable), then carve
    /// `[start, end)` out across all tracks (`DeleteInRangeEdit`, the same
    /// carve `Delete`/`Backspace` use) so undo restores it. Marquee-only,
    /// like `Delete`/`Backspace`: there is no selected-clip fallback — the
    /// view never sends this without a time selection.
    pub(super) fn cut_clips_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        start: i32,
        end: i32,
    ) {
        sequencer.copy_clips_to_clipboard(start, end);
        self.prime_os_clipboard_for_clips(sequencer);

        self.record_edit(
            sequencer,
            undo_record,
            DeleteInRangeEdit::from_time_range(sequencer, start, end),
        );
    }

    /// Cut, scoped to the marquee rectangle — the plain `⌘/Ctrl+X` binding
    /// (distinct from `cut_clips_workflow`, `Shift+⌘/Ctrl+X`'s all-track
    /// sibling that bypasses the marquee's track range). Same copy-then-delete
    /// shape, the delete being `delete_rect_workflow` — exactly what
    /// `Delete`/`Backspace` do. Marquee-only, no selected-clip fallback.
    pub(super) fn cut_clips_scoped_workflow(
        &self,
        sequencer: &mut Sequencer,
        undo_record: &mut Record<SequencerEdit>,
        rect: TimeSelectionRect,
    ) {
        sequencer.copy_clips_scoped_to_clipboard(rect);
        self.prime_os_clipboard_for_clips(sequencer);

        self.delete_rect_workflow(sequencer, undo_record, rect);
    }

    /// After a copy/cut fills `Sequencer::clip_clipboard`, prime the OS
    /// clipboard ([`prime_os_clipboard`](Self::prime_os_clipboard)).
    pub(super) fn prime_os_clipboard_for_clips(&self, sequencer: &Sequencer) {
        if sequencer.clip_clipboard().is_some() {
            self.prime_os_clipboard();
        }
    }

    /// After a copy fills the clip or note clipboard, prime the OS text
    /// clipboard with `CLIP_CLIPBOARD_SENTINEL` so the next ⌘/Ctrl+V reliably
    /// fires `egui::Event::Paste` (egui-winit only emits it when the OS
    /// clipboard holds non-empty text). The real payload stays in memory. Same
    /// cross-thread wake pattern as `110-performance-lane.md`.
    pub(super) fn prime_os_clipboard(&self) {
        if let Some(ctx) = self.repaint_ctx.get() {
            ctx.copy_text(CLIP_CLIPBOARD_SENTINEL.to_owned());
            ctx.request_repaint();
        }
    }
}
