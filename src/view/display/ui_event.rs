//! What the `"sequencer"` thread tells the UI has changed.
//!
//! Sent on `ui_event_rx`, drained by `Display` each frame. The sequencer owns
//! the authoritative model; `Display` keeps a render-side projection of it
//! (clip metadata, the open clip view, selection) and these events are how that
//! projection is kept in step. Payloads are already-rendered snapshots
//! ([`ClipMetadata`], [`ClipView`]) — the UI never reads back into sequencer
//! state. See `020-views-and-state.md`.

use uuid::Uuid;

use crate::{
    core::{config::DEFAULT_TRACK_COUNT, project::ProjectAction},
    metadata::clip_metadata::ClipMetadata,
    metadata::clip_view::ClipView,
    models::clip::EventSpaceRetime,
    models::track::{InstrumentRef, TrackShift},
};

/// A track's output as the track header shows it: [`TrackOutput`] without
/// the plugin's reference details and preset state.
///
/// [`TrackOutput`]: crate::models::track::TrackOutput
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TrackRoute {
    /// External MIDI out on `channel` (0–15).
    MidiOut {
        /// MIDI channel, 0–15.
        channel: u8,
    },
    /// A hosted instrument plugin — loaded or not (a project can name one
    /// that isn't installed, or open off macOS).
    Instrument {
        /// The plugin's display name.
        name: String,
    },
}

/// One track as the view mirrors it (`UiEvent::TracksChanged`): where its
/// output goes, the engine slot ([`Track::slot`]) its mix values and plugin
/// live in — what the header reads its faders and the plugin host its editor
/// from — the colour it draws in ([`Track::color_slot`]) and its name
/// ([`Track::name`]).
///
/// [`Track::slot`]: crate::models::track::Track::slot
/// [`Track::color_slot`]: crate::models::track::Track::color_slot
/// [`Track::name`]: crate::models::track::Track::name
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackLane {
    /// The track's engine slot.
    pub(crate) slot: usize,
    /// Where its output goes.
    pub(crate) route: TrackRoute,
    /// Which of the theme's track colours it draws in.
    pub(crate) color: usize,
    /// The user's name for it; `None` while unnamed (the header shows its
    /// number).
    pub(crate) name: Option<String>,
}

impl TrackLane {
    /// A fresh sequencer's tracks: `DEFAULT_TRACK_COUNT` of them, each in the
    /// slot, on the MIDI channel and in the colour matching its position
    /// (0-based), unnamed.
    pub(crate) fn defaults() -> Vec<TrackLane> {
        (0..DEFAULT_TRACK_COUNT)
            .map(|idx| TrackLane {
                slot: idx,
                route: TrackRoute::MidiOut { channel: idx as u8 },
                color: idx,
                name: None,
            })
            .collect()
    }
}

/// One change notification from the sequencer to `Display`. See the module docs.
pub(crate) enum UiEvent {
    /// The selected track changed to `track_idx`.
    TrackSelected {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
    },
    /// Live recording began; `clip` is the freshly created recording clip.
    RecordingStarted {
        /// Render-side snapshot of the clip.
        clip: ClipMetadata,
    },
    /// Live recording finished and committed.
    RecordingCompleted,
    /// Live recording was abandoned; the clip `clip_id` on `track_idx` is gone.
    RecordingCanceled {
        /// Target track index; out-of-range is a no-op.
        track_idx: usize,
        /// Id of the affected clip.
        clip_id: Uuid,
    },
    /// A new clip exists — add it to the render-side list.
    ClipAdded {
        /// Render-side snapshot of the clip.
        clip: ClipMetadata,
    },
    /// An existing clip's metadata (bounds, thumbnail, mute, …) changed.
    ClipUpdated {
        /// Render-side snapshot of the clip.
        clip: ClipMetadata,
    },
    /// A tempo fit (Enter) or its undo/redo moved `clip_id`'s event-tick
    /// space; follows its `ClipUpdated` and `EventsUpdated`. The open clip
    /// view maps its framing through `retime`, so the notes stay where they
    /// were on screen.
    ClipRetimed {
        /// Id of the retimed clip.
        clip_id: Uuid,
        /// How its event ticks moved.
        retime: EventSpaceRetime,
    },
    /// A clip was deleted.
    ClipRemoved {
        /// Render-side snapshot of the clip.
        clip: ClipMetadata,
    },
    /// A piano-roll view was opened; `clip_view` is its full render snapshot,
    /// `None` with no lead clip (the panel still shows, labelled empty).
    ClipEntered {
        /// Full render snapshot of the open clip view, if any.
        clip_view: Option<ClipView>,
    },
    /// The piano-roll view was closed.
    ClipExited,
    /// The lead clip — the clip under the cursor on the selected track, the
    /// one the clip view shows — changed. `None` when no clip sits there. See
    /// `archive/210-docked-clip-panel.md`.
    LeadClipChanged {
        /// Full render snapshot of the new lead clip, if any.
        clip_view: Option<ClipView>,
    },
    /// An event joined the selection.
    EventSelected {
        /// Id of the event that was selected.
        event_id: Uuid,
    },
    /// An event left the selection.
    EventDeselected {
        /// Id of the event that was deselected.
        event_id: Uuid,
    },
    /// The open clip's events changed — re-render the piano roll from
    /// `clip_view`.
    EventsUpdated {
        /// Full render snapshot of the open clip view.
        clip_view: ClipView,
    },
    /// A capture commit (`/`) inserted a take into clip `clip_id` — or a redo
    /// put it back — sent after its `EventsUpdated`
    /// (`EditResult::EventsModified::inserted_take`). If that clip is open and any row of
    /// `pitch_range` is out of view, the piano roll re-frames vertically as
    /// on opening the clip; otherwise nothing moves. Never sent for any other
    /// edit — those never re-fit the view (`220`).
    CaptureInserted {
        /// The clip the take went into.
        clip_id: Uuid,
        /// Lowest and highest pitch of the take's notes.
        pitch_range: (u8, u8),
    },
    /// A project finished loading (or a new one was created) — rebuild the
    /// render-side clip list.
    ProjectLoaded {
        /// Render-side snapshots of every clip in the project.
        clips: Vec<ClipMetadata>,
        /// The project's saved name, or `None` for a freshly created (unnamed)
        /// project — sets `Display::project`'s `project_current_name` so a later
        /// ⌘/Ctrl+S doesn't overwrite whatever was previously open.
        filename: Option<String>,
        /// The loaded project's folder (`None`: the projects root) — becomes
        /// the current folder. Meaningless for a new project, which keeps
        /// the current one.
        folder: Option<String>,
    },
    /// `action` was asked for but the project has unsaved changes: show the
    /// Save / Don't Save / Cancel prompt (`project_dialog.rs`).
    UnsavedChanges {
        /// What runs once the prompt is answered with Save or Don't Save.
        action: ProjectAction,
    },
    /// Quitting was asked for and nothing is left unsaved (or the prompt said
    /// Don't Save): close the window.
    QuitApproved,
    /// The available MIDI ports changed — and at startup, the first lists:
    /// the settings modal's port lists, kept current whether or not it is
    /// open.
    MidiPortsRefreshed {
        /// Available MIDI input port names.
        in_ports: Vec<String>,
        /// Available MIDI output port names.
        out_ports: Vec<String>,
    },
    /// The whole track list as the view mirrors it — after a project load /
    /// new-project, every output change and every track add / remove. Drives
    /// the track header's output chip and menu and its fader reads (by slot),
    /// and is how the view learns the track count.
    TracksChanged {
        /// One per track, by position — as many as there are tracks.
        tracks: Vec<TrackLane>,
        /// The add / remove that just happened, so the view moves its
        /// positional state with it (clip shapes, the marquee span); `None`
        /// for a load or an output change.
        shift: Option<TrackShift>,
    },
    /// A track left the arrangement (a remove, or the undo of an add):
    /// if it hosted a plugin, `Display` keeps the plugin's live state under
    /// `track_id` (so an undo reloads it as it was) and tears the plugin in
    /// `slot` down the usual way; with no plugin there it does nothing. macOS
    /// only; harmless elsewhere.
    TrackInstrumentRemoved {
        /// The engine slot the track held.
        slot: usize,
        /// The track's id.
        track_id: Uuid,
    },
    /// A plugin track came back into the arrangement (the undo of a remove,
    /// the redo of an add): `Display` reloads its plugin into `slot`, editor
    /// closed — from the live state it kept for `track_id` if it has one,
    /// else from `instrument`'s state blob. The project-load path. macOS
    /// only; harmless elsewhere.
    TrackInstrumentRestored {
        /// The engine slot the track is in.
        slot: usize,
        /// The track's id.
        track_id: Uuid,
        /// The plugin to reload, state blob included.
        instrument: InstrumentRef,
    },
    /// A project was loaded or created — `Display` should rebuild its hosted
    /// plugin editors to match (`specs` = the `Instrument` tracks, by engine
    /// slot, and their plugin references). Emitted on macOS only; harmless
    /// elsewhere.
    TrackInstrumentsChanged {
        /// `(engine slot, plugin reference)` for every instrument track.
        specs: Vec<(usize, InstrumentRef)>,
    },
    /// The arranger performance lane was armed (selected) — mutually
    /// exclusive with `TrackSelected`, which implicitly deselects it.
    PerformanceLaneSelected,
    /// Set the Arranger time selection to `[start, end)`. Pushed by both
    /// Duplicate workflows so the selection advances onto the fresh copy and
    /// repeated `⌘/Ctrl+D`/`Shift+⌘/Ctrl+D` chains down the timeline, and by
    /// the clip band press (`select_clip_span_workflow`) to marquee exactly
    /// one clip's span. The handler re-latches `sync_time_selection_to_cursor`'s
    /// watchers, so a cursor/track move made by the same workflow (the band
    /// press moves both) doesn't collapse the selection a frame later.
    TimeSelectionSet {
        /// Start tick of the range.
        start: i32,
        /// End tick of the range.
        end: i32,
        /// Inclusive track span, or `None` to keep whatever span is already
        /// active (defaulting to the selected track) — the Duplicate Time
        /// chain carries no track span of its own; Duplicate Clips does.
        track_span: Option<(usize, usize)>,
    },
    /// Collapse the Arranger time selection back to the track cursor —
    /// pushed by the "Delete Time" workflow once the deleted span is gone,
    /// since the marquee's tick range no longer corresponds to any real
    /// content (see `delete_time_workflow`).
    TimeSelectionCleared,
    /// A project or `.mid` was written into the projects tree (a save, a
    /// clip export): a shown browser panel re-reads it from disk.
    ProjectFilesChanged,
    /// A passing message for the footer — what an action did, or why it did
    /// nothing (the MIDI clip export). Replaces any message still showing.
    Status {
        /// The text to show.
        message: String,
    },
}
