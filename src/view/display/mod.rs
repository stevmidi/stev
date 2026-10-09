//! [`Display`] — the `eframe::App`. The whole render-side of the app.
//!
//! It holds a projection of the sequencer's state (clip metadata, the open
//! clip view, selection, mixer readers), the view-local state the sequencer
//! never sees (the modal cursors, the drag gestures, the arranger scroll), and
//! the `Arc<AtomicX>` readers for the transport. Each frame it drains
//! `ui_event_rx`, polls input via [`InputPoller`], forwards resolved
//! [`InputEvent`]s to `EventHandlers`, and paints. The submodules are the
//! concern split:
//!
//! - `input/` — the per-frame input dispatch, the drag state machines
//!   (`gestures.rs`), the modal keymaps (`modal.rs`).
//! - `rendering/` — everything painted: arranger, piano roll, timeline,
//!   overlays, the modals.
//! - `state/` — reconciling the shape lists against `UiEvent`s, scroll, the
//!   `UiEvent` drain.
//! - `instrument.rs` (macOS) — the per-track plugin editor host reached
//!   through `self.instruments`.
//! - `pane.rs` — the arranger / clip-view panes the lane area splits into;
//!   the coordinate helpers below answer for the *active* one
//!   (`active_pane`, `in_pane`). See `archive/210-docked-clip-panel.md`.
//! - `grid.rs` — the zoom-adaptive timeline grid (`GridTiers`): one pure rule
//!   for the grid lines, ruler labels, snap and cursor-follow paging.
//! - `pointer_state.rs` / `modal_focus.rs` — the small view-local state types
//!   the three subtrees above share: the pointer hover/drag/hit-test payloads
//!   and the modal focus enums. Kept here, not under a renderer, because input,
//!   rendering and the `UiEvent` drain all touch them.
//! - `midi_state.rs` / `project_state.rs` / `gesture_state.rs` /
//!   `render_state.rs` — concern-groups of `Display`'s fields folded into one
//!   sub-struct each (`Display::midi`, `Display::project`, `Display::gesture`,
//!   `Display::render`), the same shape as `instrument.rs`'s `InstrumentHost`: the driving
//!   methods stay on `Display` and reach in through the matching `self.<group>`.
//! - `status_message.rs` — the footer's passing message (`StatusMessage`).
//! - `output_menu.rs` — the track header's output menu (MIDI channel /
//!   remove the plugin), opened from a track's output chip; drawn by
//!   `rendering/output_menu.rs`.
//! - `browser.rs` — the browser side panel (`BrowserPanel`, the `BrowserTree`
//!   model) and its keyboard / pointer handling; drawn by
//!   `rendering/browser.rs`.
//!
//! See `020-views-and-state.md` for the state model and `030-ui-design.md` for
//! the layout.

mod browser;
mod gesture_state;
mod grid;
mod help_overlay;
mod input;
#[cfg(target_os = "macos")]
mod instrument;
mod midi_state;
mod modal_focus;
mod output_menu;
mod pane;
mod pointer_state;
mod project_dialog;
mod project_state;
mod render_state;
mod rendering;
mod settings_modal;
mod state;
mod status_message;
mod tempo_field;
mod track_rename;
mod ui_event;

use self::browser::{BrowserItem, BrowserPanel, BrowserPlugin};
use self::gesture_state::GestureState;
use self::grid::{GridSurface, GridTiers, grid_tiers};
use self::help_overlay::HelpOverlay;
use self::input::DragPointer;
#[cfg(target_os = "macos")]
use self::instrument::InstrumentHost;
use self::midi_state::MidiSettingsState;
use self::output_menu::OutputMenuHit;
use self::pane::{
    KeyFocus, Pane, PaneRects, PanelSize, clip_event_tick_at, clip_pane_frame_value, pane_rects,
};
use self::project_dialog::ProjectDialog;
use self::project_state::ProjectViewState;
use self::render_state::RenderState;
use self::settings_modal::SettingsModal;
use self::status_message::StatusMessage;
use self::tempo_field::TempoChip;
use self::track_rename::TrackRename;
pub(crate) use modal_focus::{MidiSettingsFocus, Overlay, PortSide, SettingsTab};
use pointer_state::{
    AxisFilter, ClipMoveDrag, ClipMoveKind, ClipResizeDrag, ClipResizeEdge, KeyZoomDrag,
    LoadedMidi, MarqueeAnchor, MidiDrag, NoteMouseDrag, NotePart, PluginDrag, TrackButton,
    TrackMixDrag, TrackMixParam, VelocityDrag,
};
pub(crate) use ui_event::{TrackLane, TrackRoute, UiEvent};

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
};

use crossbeam_channel::{Receiver, Sender};
use egui::{Color32, Rect, pos2};
use uuid::Uuid;

use crate::core::audio::AudioLoad;
#[cfg(target_os = "macos")]
use crate::core::plugin_host::PluginAudioHandle;
#[cfg(target_os = "macos")]
use crate::core::plugin_host::catalog::PluginCatalogEntry;
use crate::core::time::{pixels_to_ticks, px_per_beat_to_ppt, snap_to_grid};
use crate::{
    core::{
        event_handlers::EventHandlers,
        input_event::{InputEvent, TimeSelectionRect},
        shared_atomics::{LiveRecState, TrackMixAtomics},
        view_state::ViewState,
    },
    metadata::{
        clip_metadata::{ClipMetadata, note_thumbnails_from_spans},
        clip_view::{ClipView, EventMetadata},
    },
    shapes::{clip_shape::ClipShape, event_shape::EventShape},
    view::{input_poller::InputPoller, theme},
};

/// One clip's position atomics, shared with the clip (event-tick space) —
/// see `Display::lead_clip_time`.
struct ClipTimeAtomics {
    /// The clip's id — finds its arrangement span in `render.clip_shapes`.
    clip_id: Uuid,
    /// The clip's cursor, mirroring the transport cursor.
    cursor_tick: Arc<AtomicI32>,
    /// The clip's region start.
    region_start: Arc<AtomicI32>,
    /// The clip's region end.
    region_end: Arc<AtomicI32>,
}

impl ClipTimeAtomics {
    /// Shares `clip_view`'s position atomics.
    fn of(clip_view: &ClipView) -> Self {
        ClipTimeAtomics {
            clip_id: clip_view.clip_id,
            cursor_tick: clip_view.cursor_tick.clone(),
            region_start: clip_view.region_start.clone(),
            region_end: clip_view.region_end.clone(),
        }
    }
}

/// The `eframe::App` — the whole render side. See the module docs.
pub(crate) struct Display {
    // --- State ---
    /// Shared tempo (µs per quarter), for the header readout.
    tempo: Arc<AtomicI32>,
    /// Shared "transport running" flag.
    running: Arc<AtomicBool>,
    /// The transport odometer — the only clock the renderer needs, since the
    /// live-rec overlay measures a span rather than reading a position.
    elapsed_ticks: Arc<AtomicI32>,
    /// Shared transport playback position, ticks. The clip pane maps it into
    /// the lead clip (`lead_clip_playback_tick`).
    playback_tick: Arc<AtomicI32>,
    /// Shared transport edit-cursor position, ticks (`cursor_tick_atomic`).
    cursor_tick: Arc<AtomicI32>,
    /// Shared loop-region start (`region_start_atomic`).
    region_start: Arc<AtomicI32>,
    /// Shared loop-region end (`region_end_atomic`).
    region_end: Arc<AtomicI32>,
    /// The lead clip's own position atomics, in its event-tick space — what
    /// the clip pane reads in place of the transport's. Taken from the last
    /// `ClipView` received (`ClipEntered`, `EventsUpdated`,
    /// `LeadClipChanged`); `None` with no lead clip, when the clip pane falls
    /// back to the transport's. See `archive/210-docked-clip-panel.md`.
    lead_clip_time: Option<ClipTimeAtomics>,
    /// Shared "loop enabled" flag.
    loop_enabled: Arc<AtomicBool>,
    /// Shared active-view discriminant.
    view_state: Arc<AtomicU8>,
    /// Whether the open clip has selected events — see
    /// `SharedAtomics::has_event_selection`. Read to route the plain arrows
    /// in `Clip` (cursor step vs. note nudge).
    has_event_selection: Arc<AtomicBool>,
    /// Per-track volume / stereo balance, shared lock-free with the sequencer
    /// and CLAP mixer. Read once per frame to draw the arranger track header
    /// bars; never written here (the sequencer owns the writes — see
    /// `SequencerCommand::SetTrackVolume`).
    track_mix: Arc<TrackMixAtomics>,
    /// Audio-callback deadline utilisation, shared with the audio engine and
    /// read once per frame for the header's DSP readout. `None` when no output
    /// device was available and the app is running silent. See
    /// [`AudioLoad`].
    audio_load: Option<Arc<AudioLoad>>,

    // --- Project view state ---
    /// The ambient current project folder and name. See
    /// [`ProjectViewState`].
    project: ProjectViewState,
    /// The browser side panel — shown, focused, its Projects tree. See
    /// [`BrowserPanel`].
    browser: BrowserPanel,
    /// The window's native view, read for the pointer during a file drag
    /// from the file manager (`input/midi_drag.rs`); `None` until
    /// [`attach_drag_pointer`](Self::attach_drag_pointer), and always off
    /// macOS.
    drag_pointer: Option<DragPointer>,

    // --- Arranger selected track ---
    /// The arranger's selected track. Not project state despite the
    /// original grouping — it is read broadly (rendering, input, the CLAP
    /// host), so it stays flat on `Display`.
    selected_track_idx: usize,
    /// Which view-local surface has the keyboard: the focused pane, the
    /// track-header column or the browser panel. See [`KeyFocus`] and
    /// `020-views-and-state.md` § Views (Track header focus).
    key_focus: KeyFocus,
    /// The open track rename field, if any — it has the keyboard while open.
    /// See `track_rename.rs`.
    track_rename: Option<TrackRename>,
    /// The header's BPM chip as a control: its drag and its field (which has
    /// the keyboard while open). See `tempo_field.rs`.
    tempo_chip: TempoChip,
    /// The open Save As field or unsaved-changes prompt, if any — it has the
    /// keyboard while open. See `project_dialog.rs`.
    project_dialog: Option<ProjectDialog>,
    /// Whether a project dialog was open last frame, so its opening and
    /// closing are seen once (`sync_project_dialog_window`).
    project_dialog_shown: bool,
    /// Raise the main window next frame, once the editors have been lowered
    /// under the dialog that just opened (`sync_project_dialog_window`).
    raise_main_window: bool,
    /// Set once quitting has been approved (nothing unsaved, or the prompt
    /// said Don't Save): the window's next close request goes through
    /// instead of being held for the check. See `project_dialog.rs`.
    quit_approved: bool,
    /// Whether the last frame's input ran in the clip view, so the header
    /// focus drops on the frame the arranger loses the keyboard (an edge, not
    /// a level: a header click from the clip pane is in `Clip` until its
    /// `FocusPane` lands).
    input_was_clip_view: bool,

    // --- Arranger performance lane state ---
    /// Whether the arranger performance lane is armed (mirrors the atomic).
    performance_lane_selected: bool,

    // --- Tracks ---
    /// Each track's engine slot and output, mirrored from the sequencer
    /// (`UiEvent::TracksChanged`) for the track header — its output chip and
    /// menu (`output_menu.rs`), and the slot its faders and plugin live in.
    /// One per track, so its length is the view's track count
    /// ([`track_count`](Self::track_count)).
    tracks: Vec<TrackLane>,

    // --- Modal overlay state ---
    /// The modal overlay that is up, if any — the one switch every "does an
    /// overlay own the input?" check reads.
    overlay: Option<Overlay>,
    /// The settings modal: its tab, the theme list's scroll. See
    /// [`SettingsModal`].
    settings: SettingsModal,
    /// The settings modal's MIDI tab — port lists, the row selections and
    /// focus, connected-port badges, the output-offset mirror, and the
    /// reconnect channel ends. See [`MidiSettingsState`].
    midi: MidiSettingsState,
    /// The help overlay's scroll. See [`HelpOverlay`].
    help: HelpOverlay,

    // --- Selection / gesture state ---
    /// The view-local selection and pointer-gesture state — the arranger time
    /// selection, the event marquee, the velocity / clip-edge / track-header
    /// drags and their hover counterparts, and the in-lane cursor-line hover
    /// override. See [`GestureState`].
    gesture: GestureState,

    // --- Recording state ---
    /// The clip currently being live-recorded, if any.
    recording_clip_id: Option<Uuid>,
    /// Shared last-note / thumbnail surface for the live-rec overlay.
    live_rec_state: LiveRecState,

    // --- Input ---
    /// Polls egui's native input each frame into `InputEvent`s.
    input_poller: InputPoller,

    // --- macOS instrument plugin host ---
    /// All plugin-host state — audio-thread handle, per-track editors, pending
    /// teardown queue, plugin catalog + its background scan, repaint context.
    /// See [`InstrumentHost`]. The methods that drive it live in the
    /// `instrument` submodule as `impl Display` and reach in through
    /// `self.instruments`.
    #[cfg(target_os = "macos")]
    instruments: InstrumentHost,

    // --- Communication ---
    /// The stateless bridge to the `"sequencer"` thread.
    event_handlers: Arc<EventHandlers>,
    /// Resolved input events from `InputPoller` / the plugin key guard.
    input_event_rx: Receiver<InputEvent>,
    /// Producer end, cloned to `InputPoller` and the key guard.
    input_event_tx: Sender<InputEvent>,
    /// Change notifications from the sequencer.
    ui_event_rx: Receiver<UiEvent>,

    // --- Rendering state ---
    /// Per-frame render state — the canvas rect, the reconciled clip/event
    /// shape lists, arranger scroll + cursor-follow, and the clip pane's
    /// frozen per-frame snapshots. See [`RenderState`].
    render: RenderState,
}

impl Display {
    // --- Associated constants ---
    /// Left content padding, pixels.
    const PADDING_X: f32 = 8.0;
    /// Width of the arranger's left track-header column, holding the per-track
    /// volume and pan bars (instrument tracks only — see `030-ui-design.md`).
    /// Widening this is the single knob for the header: every content x
    /// (cursor, playhead, ruler, region, clips) routes through
    /// `content_origin_x()`. Uniform on every platform, empty off macOS.
    const TRACK_HEADER_W: f32 = 124.0;
    /// Padding between the track column's edges and the header controls (S/M
    /// row, volume/pan bars) inside it — applied equally on the left and the
    /// right by `track_header_rects` so the controls sit centred in the
    /// column rather than flush against its grid-side edge. `TRACK_HEADER_W`
    /// was widened to keep the bars a comfortable width once both sides are
    /// padded.
    const TRACK_HEADER_INNER_PAD_X: f32 = 12.0;
    /// Margin carved out of the right edge of the `TRACK_HEADER_W` reservation:
    /// the track-column fill, its separator line and the header controls all
    /// stop this far short of `content_origin_x()`, leaving a strip of bare
    /// canvas between the track column and the first bar so the grid isn't
    /// flush against the column. `content_origin_x()` itself is unchanged, so
    /// nothing in the grid moves.
    const TRACK_COLUMN_GRID_GAP_X: f32 = 8.0;
    /// Right content padding, pixels.
    const RIGHT_PADDING_X: f32 = 8.0;
    /// Width of the octave-legend column and, separately, the piano-key
    /// column in the clip view (the two are equal
    /// width, so the full keyboard margin is `2 * PIANO_MARGIN_W`).
    const PIANO_MARGIN_W: f32 = 44.0;
    /// Vertical gap between header info panel and the timeline strip.
    const HEADER_TIMELINE_GAP_Y: f32 = 4.0;
    /// Gap below the lane area, above the status bar.
    const VIEW_BOTTOM_MARGIN_Y: f32 = 8.0;
    /// Height of the dedicated timeline/region strip above the clip lanes.
    /// At 22 the three stacked bands (time-selection markers ~top, bar
    /// numbers ~middle, region band at the bottom) overlapped vertically — the
    /// 12px bar-number glyphs, bottom-anchored above the region band, reached
    /// to within 2px of the strip top where the markers live. 30 gives the
    /// marker band and the number band real clearance now that the region
    /// band is `REGION_BAND_H` (10px) and the numbers ride on top of it; the
    /// ~1px it costs each of the 8 lanes is unnoticeable.
    const TIMELINE_H: f32 = 30.0;
    /// Height of the reserved arranger performance lane row, drawn between
    /// the timeline strip and track 1.
    const PERFORMANCE_LANE_H: f32 = 24.0;

    // --- Constructor ---
    #[allow(clippy::too_many_arguments)]
    /// Builds the app with its channel ends and shared-atomic readers; all view-local state starts empty. Parameter order: channels, then flat values, then atomics.
    pub(crate) fn new(
        event_handlers: Arc<EventHandlers>,
        input_event_rx: Receiver<InputEvent>,
        input_event_tx: Sender<InputEvent>,
        ui_event_rx: Receiver<UiEvent>,
        midi_in_reconnect_tx: Sender<String>,
        midi_out_reconnect_tx: Sender<String>,
        midi_in_current_port: Option<String>,
        midi_out_current_port: Option<String>,
        last_project_folder: Option<String>,
        midi_out_offset_ms: i32,
        tempo: Arc<AtomicI32>,
        running: Arc<AtomicBool>,
        elapsed_ticks: Arc<AtomicI32>,
        playback_tick: Arc<AtomicI32>,
        cursor_tick: Arc<AtomicI32>,
        region_start: Arc<AtomicI32>,
        region_end: Arc<AtomicI32>,
        loop_enabled: Arc<AtomicBool>,
        view_state: Arc<AtomicU8>,
        has_event_selection: Arc<AtomicBool>,
        track_mix: Arc<TrackMixAtomics>,
        live_rec_state: LiveRecState,
    ) -> Self {
        Display {
            tempo,
            running,
            elapsed_ticks,
            playback_tick,
            cursor_tick,
            region_start,
            region_end,
            lead_clip_time: None,
            loop_enabled,
            view_state,
            has_event_selection,
            track_mix,
            audio_load: None,
            project: ProjectViewState::new(last_project_folder),
            browser: BrowserPanel::default(),
            drag_pointer: None,
            selected_track_idx: 0,
            key_focus: KeyFocus::Pane,
            track_rename: None,
            tempo_chip: TempoChip::default(),
            project_dialog: None,
            project_dialog_shown: false,
            raise_main_window: false,
            quit_approved: false,
            input_was_clip_view: false,
            performance_lane_selected: false,
            overlay: None,
            settings: SettingsModal::default(),
            help: HelpOverlay::default(),
            tracks: TrackLane::defaults(),
            midi: MidiSettingsState::new(
                midi_in_reconnect_tx,
                midi_out_reconnect_tx,
                midi_in_current_port,
                midi_out_current_port,
                midi_out_offset_ms,
            ),
            gesture: GestureState::default(),
            recording_clip_id: None,
            live_rec_state,
            input_poller: InputPoller::new(),
            #[cfg(target_os = "macos")]
            instruments: InstrumentHost::new(),
            event_handlers,
            input_event_rx,
            input_event_tx,
            ui_event_rx,
            render: RenderState::new(),
        }
    }

    /// Hands over the running plugin-host audio thread. Called once from `main`
    /// on the eframe main thread. Plugins are loaded per-track later, from the
    /// browser panel.
    #[cfg(target_os = "macos")]
    pub(crate) fn attach_instrument_audio(
        &mut self,
        handle: PluginAudioHandle,
        live_instrument_target: Arc<AtomicI32>,
    ) {
        self.instruments.audio = Some(handle);
        self.instruments.live_instrument_target = live_instrument_target;
    }

    /// Hands over the audio engine's load meter, so the header can show how
    /// much of each audio block's deadline the render is using. Called once
    /// from `main` when the engine started; left `None` if it didn't.
    pub(crate) fn attach_audio_load(&mut self, load: Arc<AudioLoad>) {
        self.audio_load = Some(load);
    }

    /// Hands over the receiving end of the background plugin-catalog scan.
    /// Called once from `main`, alongside `attach_instrument_audio`.
    #[cfg(target_os = "macos")]
    pub(crate) fn attach_plugin_catalog_rx(&mut self, rx: Receiver<Vec<PluginCatalogEntry>>) {
        self.instruments.plugin_catalog_rx = Some(rx);
    }

    // --- Core state accessors / mutators ---
    /// The active view.
    fn view_state(&self) -> ViewState {
        ViewState::from_u8(self.view_state.load(Ordering::Relaxed))
    }

    /// Whether the open clip has selected events (the sequencer-published
    /// mirror).
    fn has_event_selection(&self) -> bool {
        self.has_event_selection.load(Ordering::Relaxed)
    }

    /// Whether a live-record take is in progress.
    fn is_recording(&self) -> bool {
        self.recording_clip_id.is_some()
    }

    /// Whether playback loops at the region end.
    fn is_loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Relaxed)
    }

    /// Reads region bounds as a consistent pair.
    ///
    /// `region_start` and `region_end` are stored in separate atomics and can
    /// be updated independently. Reading them once each can produce a transient
    /// mixed pair for one frame. Re-read until both values are stable.
    fn region_bounds_snapshot(&self) -> (i32, i32) {
        loop {
            let start_1 = self.region_start_atomic().load(Ordering::Relaxed);
            let end_1 = self.region_end_atomic().load(Ordering::Relaxed);
            let start_2 = self.region_start_atomic().load(Ordering::Relaxed);
            let end_2 = self.region_end_atomic().load(Ordering::Relaxed);

            if start_1 == start_2 && end_1 == end_2 {
                return (start_1, end_1);
            }
        }
    }

    /// This frame's region bounds for the active pane — the clip pane's
    /// frozen snapshot if one was taken (`clip_pane_frame_value`), else a live
    /// snapshot.
    fn region_bounds_for_render(&self) -> (i32, i32) {
        clip_pane_frame_value(self.active_pane(), self.render.clip_frame_region_bounds)
            .unwrap_or_else(|| self.region_bounds_snapshot())
    }

    /// The lead clip's position atomics when the clip pane is active and
    /// there is a lead clip; `None` means "use the transport's".
    fn pane_clip_time(&self) -> Option<&ClipTimeAtomics> {
        match self.active_pane() {
            Pane::Clip => self.lead_clip_time.as_ref(),
            Pane::Arranger => None,
        }
    }

    /// The active pane's cursor: the lead clip's in the clip pane, the
    /// transport's in the arranger.
    fn cursor_tick_atomic(&self) -> &AtomicI32 {
        self.pane_clip_time()
            .map_or(&self.cursor_tick, |time| &time.cursor_tick)
    }

    /// The active pane's region start: the lead clip's region in the clip
    /// pane, the loop region in the arranger.
    fn region_start_atomic(&self) -> &AtomicI32 {
        self.pane_clip_time()
            .map_or(&self.region_start, |time| &time.region_start)
    }

    /// The active pane's region end (see `region_start_atomic`).
    fn region_end_atomic(&self) -> &AtomicI32 {
        self.pane_clip_time()
            .map_or(&self.region_end, |time| &time.region_end)
    }

    /// Closes the modal overlay that is up, if any.
    pub(super) fn close_overlay(&mut self) {
        self.overlay = None;
    }

    /// The pane with the keyboard: the clip view in the clip view, the
    /// arranger in the arranger (the settings modal, an overlay, changes
    /// neither).
    fn focused_pane(&self) -> Pane {
        if self.view_state().is_clip_view() {
            Pane::Clip
        } else {
            Pane::Arranger
        }
    }

    /// Whether the track-header column has the keyboard: focused by a click,
    /// with the arranger the focused pane.
    fn track_headers_have_keyboard(&self) -> bool {
        self.key_focus == KeyFocus::TrackHeaders && self.view_state() == ViewState::Arranger
    }

    /// The pane the coordinate helpers answer for: the one an `in_pane`
    /// scope set, else the focused one.
    fn active_pane(&self) -> Pane {
        self.render
            .pane_override
            .unwrap_or_else(|| self.focused_pane())
    }

    /// `⌘⌥E`: flips the clip panel between maximized and docked. Hidden, it
    /// only changes the size the panel will show at.
    fn toggle_clip_panel_size(&mut self) {
        let panel = &mut self.render.clip_panel;
        panel.size = match panel.size {
            PanelSize::Maximized => PanelSize::Docked,
            PanelSize::Docked => PanelSize::Maximized,
        };
    }

    /// Runs `f` with `pane` as the active pane, restoring the previous one
    /// after — a draw pass or hit-test scoped to one pane.
    fn in_pane<R>(&mut self, pane: Pane, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.render.pane_override.replace(pane);
        let result = f(self);
        self.render.pane_override = previous;
        result
    }

    /// Each visible pane's rect this frame, per the clip panel's layout.
    fn pane_rects(&self) -> PaneRects {
        pane_rects(self.lane_area_rect(), self.render.clip_panel.layout())
    }

    /// Whether `pane` is on screen.
    fn is_pane_visible(&self, pane: Pane) -> bool {
        self.pane_rects().get(pane).is_some()
    }

    /// The visible pane under the pointer at `(x, y)`, `None` over the
    /// header or status bar or outside the window.
    fn pane_at(&self, x: f32, y: f32) -> Option<Pane> {
        if !self.render.canvas_rect.contains(pos2(x, y)) {
            return None;
        }
        self.pane_rects().pane_at_y(y)
    }

    /// The lane area both panes share: below the header, above the status
    /// bar, the full canvas width. Relative to the canvas origin, like every
    /// y the renderers add `rect.min.y` to.
    fn lane_area_rect(&self) -> Rect {
        Rect::from_min_max(
            pos2(
                self.render.canvas_rect.min.x,
                theme::HEADER_H + Self::HEADER_TIMELINE_GAP_Y,
            ),
            pos2(
                self.render.canvas_rect.max.x,
                self.render.canvas_rect.max.y - theme::STATUS_H - Self::VIEW_BOTTOM_MARGIN_Y,
            ),
        )
    }

    /// The active pane's rect. Falls back to the whole lane area for a
    /// hidden pane, so a stray hit-test there still gets sane geometry.
    fn pane_rect(&self) -> Rect {
        self.pane_rects()
            .get(self.active_pane())
            .unwrap_or_else(|| self.lane_area_rect())
    }

    /// The active pane's horizontal scroll offset.
    fn scroll_x(&self) -> f32 {
        match self.active_pane() {
            Pane::Arranger => self.render.arranger_scroll_x,
            Pane::Clip => self.render.clip_scroll_x,
        }
    }

    /// Scroll offset to render with (frozen for the clip pane's draw pass).
    fn render_scroll_x(&self) -> f32 {
        clip_pane_frame_value(self.active_pane(), self.render.clip_frame_scroll_x)
            .unwrap_or_else(|| self.scroll_x())
    }

    /// Cursor tick to render with (frozen for the clip pane's draw pass).
    fn render_cursor_tick(&self) -> i32 {
        clip_pane_frame_value(self.active_pane(), self.render.clip_frame_cursor_tick)
            .unwrap_or_else(|| self.cursor_tick_atomic().load(Ordering::Relaxed))
    }

    /// The active pane's playback position: the transport's in the arranger,
    /// and in the clip view the transport playhead mapped into the lead clip (`lead_clip_playback_tick`) — `None` while it is outside
    /// the clip, or there is no lead clip.
    fn pane_playback_tick(&self) -> Option<i32> {
        match self.active_pane() {
            Pane::Arranger => Some(self.playback_tick()),
            Pane::Clip => self.lead_clip_playback_tick(),
        }
    }

    /// The transport playhead in the lead clip's event-tick space, `None`
    /// while it is outside the clip's arrangement span. The clip view no
    /// longer loops the transport on its clip, so the clip's own playback
    /// counter stalls whenever playback runs elsewhere — see
    /// `archive/210-docked-clip-panel.md`.
    fn lead_clip_playback_tick(&self) -> Option<i32> {
        let lead = self.lead_clip_time.as_ref()?;
        let shape = self
            .render
            .clip_shapes
            .iter()
            .find(|shape| shape.clip_id() == lead.clip_id)?;
        clip_event_tick_at(
            self.playback_tick(),
            (shape.start_tick(), shape.end_tick()),
            (
                lead.region_start.load(Ordering::Relaxed),
                lead.region_end.load(Ordering::Relaxed),
            ),
        )
    }

    /// Whether the transport is running, as this frame renders it: the clip
    /// pane's per-frame snapshot, taken with its playhead
    /// (`clip_frame_running`), else the live flag.
    fn render_running(&self) -> bool {
        clip_pane_frame_value(self.active_pane(), self.render.clip_frame_running)
            .unwrap_or_else(|| self.running.load(Ordering::Relaxed))
    }

    /// Playback tick to render with (frozen for the clip pane's draw pass), `None` where
    /// the active pane has no playhead to show (see `pane_playback_tick`).
    fn render_playback_tick(&self) -> Option<i32> {
        clip_pane_frame_value(self.active_pane(), self.render.clip_frame_playback_tick)
            .or_else(|| self.pane_playback_tick())
    }

    /// Horizontal scale for the active pane.
    fn pixels_per_tick(&self) -> f32 {
        px_per_beat_to_ppt(match self.active_pane() {
            Pane::Clip => self.clip_px_per_beat(),
            Pane::Arranger => self.arranger_px_per_beat(),
        })
    }

    /// Reciprocal of [`pixels_per_tick`](Self::pixels_per_tick).
    fn ticks_per_pixel(&self) -> f32 {
        1.0 / self.pixels_per_tick()
    }

    /// Screen x → absolute tick, snapped by the caller.
    fn screen_x_to_tick(&self, screen_x: f32) -> i32 {
        let timeline_px = (screen_x + self.scroll_x() - self.content_origin_x()).max(0.0);
        pixels_to_ticks(timeline_px, self.ticks_per_pixel())
    }

    /// Screen x → absolute tick snapped to the pointer grid
    /// (`cursor_grid_ticks`).
    fn snapped_tick_at(&self, screen_x: f32) -> i32 {
        snap_to_grid(self.screen_x_to_tick(screen_x), self.cursor_grid_ticks())
    }

    /// Inverse of `screen_x_to_tick`: maps an absolute tick to its rounded screen
    /// x. Uses `scroll_x()` rather than `render_scroll_x()` so the two
    /// conversions stay exact inverses of each other in every view.
    fn tick_to_screen_x(&self, tick: i32) -> f32 {
        self.tick_to_x(tick).round()
    }

    /// [`tick_to_screen_x`](Self::tick_to_screen_x) unrounded — for a body
    /// whose far edge is measured from it (clip and note bodies, thumbnails).
    fn tick_to_x(&self, tick: i32) -> f32 {
        tick as f32 * self.pixels_per_tick() + self.content_origin_x() - self.scroll_x()
    }

    /// Drops the time selection along with any drag in progress. The selection is
    /// scoped to a single arranger visit, so leaving the view discards it.
    fn clear_time_selection(&mut self) {
        self.gesture.time_selection = None;
        self.gesture.time_selection_anchor = None;
    }

    /// Drops the hover cursor line and every view-local gesture — selection,
    /// marquee, drags and their hovers. On every clip entry and exit and on
    /// project load.
    fn clear_gestures(&mut self) {
        self.gesture.hover_cursor = None;
        self.clear_time_selection();
        self.clear_event_marquee();
        self.clear_note_drag();
        self.clear_clip_resize();
        self.clear_clip_move_drag();
        self.clear_track_mix_drag();
    }

    /// Re-arms the arranger's cursor-follow paging, so it pages to the
    /// cursor on the next arranger frame.
    fn rearm_arranger_follow(&mut self) {
        self.render.arranger_follow_suspended = false;
        self.render.arranger_follow_last_cursor_tick = None;
    }

    /// The shape of `(track_idx, clip_id)`, if present.
    fn clip_shape(&self, track_idx: usize, clip_id: Uuid) -> Option<&ClipShape> {
        self.render
            .clip_shapes
            .iter()
            .find(|shape| shape.matches(track_idx, clip_id))
    }

    /// The piano-roll shape of note `event_id`, if present.
    fn event_shape(&self, event_id: Uuid) -> Option<&EventShape> {
        self.render
            .event_shapes
            .iter()
            .find(|shape| shape.matches(event_id))
    }

    /// What a drag pressed on note `pressed_id` acts on — Ableton's rule,
    /// shared by the velocity and note move / resize drags: the whole
    /// selection when the pressed note is in it, else the pressed note alone
    /// (the real selection untouched). Returns whether the pressed note was
    /// selected, and the targets.
    fn drag_targets(&self, pressed_id: Uuid) -> (bool, Vec<Uuid>) {
        if self
            .event_shape(pressed_id)
            .is_some_and(|shape| shape.is_selected())
        {
            let selected = self
                .render
                .event_shapes
                .iter()
                .filter(|shape| shape.is_selected())
                .map(|shape| shape.event_id())
                .collect();
            (true, selected)
        } else {
            (false, vec![pressed_id])
        }
    }

    /// Drops any in-progress note move / resize drag and its hover state.
    fn clear_note_drag(&mut self) {
        self.gesture.note_drag = None;
        self.gesture.note_hover = None;
    }

    /// Drops any in-progress clip edge resize drag and its hover state. Like
    /// `clear_time_selection`, scoped to a single arranger visit.
    fn clear_clip_resize(&mut self) {
        self.gesture.clip_resize_drag = None;
        self.gesture.clip_resize_hover = None;
    }

    /// Drops any in-progress clip band (move) drag and its hover state
    /// without committing anything — the ghost simply vanishes. Like
    /// `clear_clip_resize`, scoped to a single arranger visit; also the Esc
    /// cancel.
    fn clear_clip_move_drag(&mut self) {
        self.gesture.clip_move_drag = None;
        self.gesture.clip_move_hover = None;
    }

    /// Drops any in-progress track-header volume/pan drag and its hover state,
    /// plus the S/M button hover. Like `clear_clip_resize`, scoped to a single
    /// arranger visit.
    fn clear_track_mix_drag(&mut self) {
        self.gesture.track_mix_drag = None;
        self.gesture.track_mix_hover = None;
        self.gesture.track_button_hover = None;
    }

    /// The engine slot of the track at `track_idx` (`TrackLane::slot`) —
    /// where its mix values and plugin live. `None` out of range.
    fn track_slot(&self, track_idx: usize) -> Option<usize> {
        self.tracks.get(track_idx).map(|lane| lane.slot)
    }

    /// The accent colour of the track at `track_idx` — its own colour slot
    /// (`TrackLane::color`), so it stays with the track when one above it is
    /// added or removed. The position's colour out of range.
    fn track_color(&self, track_idx: usize) -> Color32 {
        theme::track_color(
            self.tracks
                .get(track_idx)
                .map_or(track_idx, |lane| lane.color),
        )
    }

    /// The accent colour of the lead clip's track — the clip view's notes.
    /// The lead clip is always on the selected track.
    fn lead_clip_color(&self) -> Color32 {
        self.track_color(self.selected_track_idx)
    }

    /// Reads `track_idx`'s mute flag from its slot's shared atomics.
    fn track_muted(&self, track_idx: usize) -> bool {
        self.track_slot(track_idx)
            .is_some_and(|slot| self.track_mix.muted(slot))
    }

    /// Reads `track_idx`'s solo flag from its slot's shared atomics.
    fn track_soloed(&self, track_idx: usize) -> bool {
        self.track_slot(track_idx)
            .is_some_and(|slot| self.track_mix.soloed(slot))
    }

    /// Reads `track_idx`'s mixer volume (dB) from its slot's shared atomics.
    /// `0.0` (unity) for an out-of-range index or an untouched track.
    fn track_volume_db(&self, track_idx: usize) -> f32 {
        self.track_slot(track_idx)
            .map_or(0.0, |slot| self.track_mix.volume_db(slot))
    }

    /// Reads `track_idx`'s stereo balance (`-1.0`..=`1.0`, `0.0` = centre).
    fn track_pan(&self, track_idx: usize) -> f32 {
        self.track_slot(track_idx)
            .map_or(0.0, |slot| self.track_mix.pan(slot))
    }

    /// How many tracks the project has — one lane per track in the mirror
    /// (`UiEvent::TracksChanged`), so it is never zero.
    fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// True when `track_idx` currently hosts a plugin instrument, so its header
    /// shows volume/pan bars. Always `false` off macOS (no plugin host).
    fn track_has_instrument(&self, track_idx: usize) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.track_instrument(track_idx).is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = track_idx;
            false
        }
    }

    /// Drops the event marquee box along with any drag in progress. Unlike
    /// `clear_time_selection`, this never touches the actual event
    /// selection — that lives in `Clip::event_selection` and is meant to
    /// persist after the drag ends; this only clears the view-local
    /// rendering/drag state so no stale box survives a view change.
    fn clear_event_marquee(&mut self) {
        self.gesture.event_marquee_anchor = None;
        self.gesture.event_marquee_rect = None;
    }

    /// The grid visible in the active pane: zoom-adaptive everywhere
    /// (`grid_tiers` at the pane's scale) — the arranger's surface in the
    /// arranger (never finer than a 16th), the piano roll's in the clip view
    /// (Ableton-Narrowest density, down to a 256th zoomed in; see
    /// `GridSurface`). Drives the grid
    /// lines, ruler labels, snap and cursor-follow paging alike — see
    /// `grid.rs`.
    fn grid_tiers(&self) -> GridTiers {
        let surface = match self.active_pane() {
            Pane::Clip => GridSurface::PianoRoll,
            Pane::Arranger => GridSurface::Arranger,
        };
        grid_tiers(self.pixels_per_tick(), surface.snap_floor_ticks(), surface)
    }

    /// Grid resolution (in ticks) that every mouse gesture snaps to for the
    /// current view — cursor-line hover, click placement, time-selection
    /// drag edges, and clip-resize drag edges (`gestures.rs`). It is the finest *visible* grid tier
    /// (`GridTiers::snap_ticks`), so it follows the zoom. The keyboard's plain
    /// Left/Right step matches this same resolution (`MoveCursorByGrid` →
    /// `TransportCommand::MoveCursor` in the arranger,
    /// `SequencerCommand::NudgeClipCursorByGrid` in `Clip`), just walking one
    /// grid line per press instead of landing on whichever is nearest to the
    /// pointer.
    fn cursor_grid_ticks(&self) -> i32 {
        self.grid_tiers().snap_ticks
    }

    /// The transport odometer value.
    fn elapsed_tick(&self) -> i32 {
        self.elapsed_ticks.load(Ordering::Relaxed)
    }

    /// The current transport playback tick.
    fn playback_tick(&self) -> i32 {
        self.playback_tick.load(Ordering::Relaxed)
    }

    // --- Layout helpers ---
    /// Top y of the active pane (its timeline strip's top edge).
    fn track_area_top(&self) -> f32 {
        self.pane_rect().min.y
    }

    /// Bottom y of the active pane.
    fn track_area_bottom(&self) -> f32 {
        self.pane_rect().max.y
    }

    /// Height of the active pane.
    fn track_area_h(&self) -> f32 {
        self.track_area_bottom() - self.track_area_top()
    }

    /// Left x where timeline content starts (after the track header / piano
    /// gutter of the active pane).
    fn content_origin_x(&self) -> f32 {
        match self.active_pane() {
            Pane::Clip => Self::PADDING_X + Self::PIANO_MARGIN_W * 2.0,
            Pane::Arranger => Self::PADDING_X + Self::TRACK_HEADER_W,
        }
    }

    /// Right x where timeline content ends.
    fn content_right_x(&self) -> f32 {
        self.render.canvas_rect.max.x - Self::RIGHT_PADDING_X
    }
    /// Width of the timeline content area.
    fn content_w(&self) -> f32 {
        (self.content_right_x() - self.content_origin_x()).max(1.0)
    }

    /// True when screen-x `x` lies within the content area — between the left
    /// track-header / piano gutter and the right padding. Full-height overlay
    /// lines (the cursor line, the playhead) are skipped when it's false, so a
    /// scrolled-away tick doesn't paint a stray line into the gutter.
    fn content_x_on_screen(&self, x: f32) -> bool {
        (self.content_origin_x()..=self.content_right_x()).contains(&x)
    }
}
