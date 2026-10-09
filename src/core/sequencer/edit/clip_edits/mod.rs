//! Undoable operations on whole clips — one `impl undo::Edit` type per file.
//!
//! Each `*Edit` captures enough state at `from_*` construction to both apply
//! and reverse the change, and each `edit()`/`undo()` returns an
//! [`EditResult`] describing exactly which clips were
//! updated / added / removed so the handler fires minimal UI events. The
//! compound edits (`InsertSilenceEdit`, `DeleteTimeEdit`, `DuplicateTimeEdit`,
//! `DuplicateClipsEdit`, `PasteClipsEdit` (also Merge Clips and the MIDI clip import), `MuteInRangeEdit`, `MoveClipEdit`,
//! `MoveRangeEdit`) are built by composing the simpler ones (`SplitClipsEdit`,
//! `DeleteInRangeEdit`); `ResizeClipEdit` moves one clip's edges
//! and nothing else, and `RetimeClipEdit` retimes one clip with the tempo
//! (Enter's fit, `⌥=`/`⌥-`); `CommitClipEdit` is the one that puts a
//! brand-new clip (a capture commit, a live take) on
//! a track. See `050-undo-redo.md` and `020-views-and-state.md`.

use std::collections::HashSet;

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;
use crate::models::region::Region;

use super::super::Sequencer;
use super::EditResult;

mod commit_clip;
mod delete_in_range;
mod delete_time;
mod duplicate_clips;
mod duplicate_time;
mod import_clip;
mod insert_silence;
mod merge_clips;
mod move_clip;
mod move_range;
mod mute_in_range;
mod paste;
mod resize_clip;
mod retime_clip;
mod split;

pub(crate) use commit_clip::CommitClipEdit;
pub(crate) use delete_in_range::DeleteInRangeEdit;
pub(crate) use delete_time::DeleteTimeEdit;
pub(crate) use duplicate_clips::DuplicateClipsEdit;
pub(crate) use duplicate_time::DuplicateTimeEdit;
pub(crate) use insert_silence::InsertSilenceEdit;
pub(crate) use move_clip::MoveClipEdit;
pub(crate) use move_range::MoveRangeEdit;
pub(crate) use mute_in_range::MuteInRangeEdit;
pub(crate) use paste::PasteClipsEdit;
pub(crate) use resize_clip::ResizeClipEdit;
pub(crate) use retime_clip::RetimeClipEdit;
pub(crate) use split::SplitClipsEdit;

/// `Clip::clone` with a freshly-constructed `Region`, breaking the
/// `Arc<AtomicI32>` sharing a plain clone keeps (see `050-undo-redo.md`). Any
/// edit that freezes a `Clip` snapshot and later re-adds it must go through
/// this, or a region edit on the live clip silently rewrites the snapshot.
pub(super) fn detached_clone(clip: &Clip) -> Clip {
    detach(clip.clone())
}

/// [`detached_clone`] for a clip already owned (just lifted off its track):
/// swaps in a fresh `Region` without copying the event list.
fn detach(mut clip: Clip) -> Clip {
    *clip.region_mut() = Region::new(clip.region().start(), clip.region().end());
    clip
}

/// Whether `track_idx` lies in an inclusive `(lo, hi)` track span; `None`
/// means every track.
fn in_tracks(track_filter: Option<(usize, usize)>, track_idx: usize) -> bool {
    track_filter.is_none_or(|(lo, hi)| (lo..=hi).contains(&track_idx))
}

/// `(track index, clip)` for the clip with this id, searching every track.
fn find_clip(sequencer: &Sequencer, id: Uuid) -> Option<(usize, &Clip)> {
    sequencer
        .tracks()
        .iter()
        .enumerate()
        .find_map(|(track_idx, track)| track.get_clip_by_id(id).map(|clip| (track_idx, clip)))
}

/// Current metadata of every clip in `ids` (searching every track), in
/// order, minus `exclude` and duplicates — both edge splits of a range edit
/// report the same original.
fn metadata_for<'a>(
    sequencer: &Sequencer,
    ids: impl IntoIterator<Item = &'a Uuid>,
    exclude: &HashSet<Uuid>,
) -> Vec<ClipMetadata> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| !exclude.contains(id) && seen.insert(**id))
        .filter_map(|id| find_clip(sequencer, *id))
        .map(|(track_idx, clip)| ClipMetadata::from_clip(track_idx, clip))
        .collect()
}

/// `(track, id)` of every clip on the filtered tracks lying fully inside
/// `[start, end)`, in track order.
fn interior_clip_ids(
    sequencer: &Sequencer,
    track_filter: Option<(usize, usize)>,
    start: i32,
    end: i32,
) -> Vec<(usize, Uuid)> {
    sequencer
        .tracks()
        .iter()
        .enumerate()
        .filter(|&(track_idx, _)| in_tracks(track_filter, track_idx))
        .flat_map(|(track_idx, track)| {
            track
                .find_clip_ids_within(start, end)
                .into_iter()
                .map(move |id| (track_idx, id))
        })
        .collect()
}

/// Current metadata of each still-present `targets` clip, split into
/// `(in ids, not in ids)` — the ripple edits' "split/carve piece vs. plain
/// shift" buckets.
fn partition_metadata(
    sequencer: &Sequencer,
    targets: &[(usize, Uuid)],
    ids: &HashSet<Uuid>,
) -> (Vec<ClipMetadata>, Vec<ClipMetadata>) {
    let mut in_ids = Vec::new();
    let mut rest = Vec::new();
    for &(track_idx, clip_id) in targets {
        let Some(clip) = sequencer.clip_on(track_idx, clip_id) else {
            continue;
        };
        let metadata = ClipMetadata::from_clip(track_idx, clip);
        if ids.contains(&clip_id) {
            in_ids.push(metadata);
        } else {
            rest.push(metadata);
        }
    }
    (in_ids, rest)
}

/// The three `carve_*` buckets a compound edit gathers from the
/// `DeleteInRangeEdit`s it composes.
#[derive(Default)]
struct CarveBuckets {
    /// Trimmed originals (ids kept).
    updated: Vec<ClipMetadata>,
    /// New right-hand pieces (forward) / cleared clips reappearing (undo).
    added: Vec<ClipMetadata>,
    /// Clips cleared whole (forward) / right-hand pieces deleted again (undo).
    removed: Vec<ClipMetadata>,
}

impl CarveBuckets {
    /// Appends a carve's `RangeDeleted`/`RangeRestored` buckets; any other
    /// result (a `NoOp`) adds nothing.
    fn absorb(&mut self, result: EditResult) {
        if let EditResult::RangeDeleted {
            updated,
            added,
            removed,
            ..
        }
        | EditResult::RangeRestored {
            updated,
            added,
            removed,
            ..
        } = result
        {
            self.updated.extend(updated);
            self.added.extend(added);
            self.removed.extend(removed);
        }
    }

    /// `true` when no carve touched anything.
    fn is_empty(&self) -> bool {
        self.updated.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Sequencer helpers shared by the clip edits
// ---------------------------------------------------------------------------

impl Sequencer {
    /// Removes `clip_id` from `track_idx`, first queuing its sounding notes
    /// for release when the transport is running (a clip pulled out from
    /// under an open note would otherwise strand its note-off; covers the
    /// CLAP instrument route, which the `NoteLogger` net misses). Returns the
    /// clip's metadata as it was on that track plus the clip itself, or
    /// `None` if it isn't there.
    fn lift_clip(&mut self, track_idx: usize, clip_id: Uuid) -> Option<(ClipMetadata, Clip)> {
        let running = self.is_running();
        let track = self.tracks_mut().get_mut(track_idx)?;
        if running {
            track.release_sounding_notes_for_clip(clip_id);
        }
        let clip = track.remove_clip_by_id(clip_id)?;
        Some((ClipMetadata::from_clip(track_idx, &clip), clip))
    }

    /// `(track, id)` of every clip, on every track, starting at or after
    /// `tick` — the clips a ripple edit shifts.
    fn clips_starting_at_or_after(&self, tick: i32) -> Vec<(usize, Uuid)> {
        self.tracks()
            .iter()
            .enumerate()
            .flat_map(|(track_idx, track)| {
                track
                    .clips()
                    .iter()
                    .filter(move |clip| clip.start_tick() >= tick)
                    .map(move |clip| (track_idx, clip.id()))
            })
            .collect()
    }

    /// Moves every `targets` clip by `delta` ticks with a raw
    /// `set_start_tick` (see `InsertSilenceEdit` for why that can't create an
    /// overlap). Shifting a clip that is currently sounding moves it out
    /// from under the playhead, which strands any open note the same way a
    /// shrink or removal would, so its notes are released first.
    fn shift_clips(&mut self, targets: &[(usize, Uuid)], delta: i32) {
        let running = self.is_running();
        for &(track_idx, clip_id) in targets {
            let Some(track) = self.tracks_mut().get_mut(track_idx) else {
                continue;
            };
            if running {
                track.release_sounding_notes_for_clip(clip_id);
            }
            if let Some(clip) = track.get_clip_by_id_mut(clip_id) {
                clip.set_start_tick(clip.start_tick() + delta);
            }
        }
    }
}
