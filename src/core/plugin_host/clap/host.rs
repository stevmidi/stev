//! The host-side callback handlers for the CLAP format. [`StevClapHost`]
//! declares these host extensions:
//!
//! - **log** — plugin log messages are routed to `debug!`, which any
//!   thread may call, the audio thread included, so they never reach a
//!   release log.
//! - **gui** — floating-window lifecycle callbacks (`closed`, resize requests).
//! - **timer** — the plugin registers periodic timers that the UI thread ticks
//!   from [`ClapEditor::pump`](crate::core::plugin_host::InstrumentEditor::pump).
//! - **state** — the plugin flags unsaved changes (`mark_dirty`), which the
//!   unsaved prompt counts.
//! - **params** — `request_flush` is `request_process`: the engine's callback
//!   never stops, so waking the voice gets the plugin its `process` call, which
//!   does what a flush would, and `flush` itself is never called. `rescan` marks
//!   the values for [`ClapEditor`](super::ClapEditor) to read again. See
//!   [`params`](super::params).
//! - **thread-check** — the plugin asks whether it is on the main or the audio
//!   thread. Main = the AppKit main thread; audio = any thread marked by
//!   [`enter_render_thread`](crate::core::audio::enter_render_thread) (the `cpal`
//!   callback and every `WorkerPool` worker). Without it u-he plugins log
//!   "Cannot use thread checks!" on every host callback.
//!
//! All GUI / timer / callback pumping happens on the eframe **main thread** (the
//! only thread with an AppKit run loop). The audio processor keeps running on
//! the audio-engine callback thread and never touches any of this.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl, PluginGui};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
    PluginParams,
};
use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::prelude::*;
use log::debug;
use objc2::MainThreadMarker;

use crate::core::audio::is_audio_thread;
use crate::core::plugin_host::ui_wake::UiWake;

/// The `HostHandlers` implementation for the hosted CLAP instrument.
pub(crate) struct StevClapHost;

impl HostHandlers for StevClapHost {
    type Shared<'a> = ClapHostShared;
    type MainThread<'a> = ClapHostMainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostGui>()
            .register::<HostTimer>()
            .register::<HostState>()
            .register::<HostParams>()
            .register::<HostThreadCheck>();
    }
}

/// `[thread-safe]` host state. Holds the plugin extension pointers (queried once
/// during init) and the cross-thread signals the UI thread drains each frame.
pub(crate) struct ClapHostShared {
    /// Set by `request_callback` (possibly from the audio thread); drained by
    /// [`take_callback_request`](Self::take_callback_request).
    callback_requested: AtomicBool,
    /// Set by `request_process` (any thread); drained on the audio thread by
    /// [`take_process_request`](Self::take_process_request).
    process_requested: AtomicBool,
    /// Set by [`HostGuiImpl::closed`]; drained by
    /// [`take_gui_closed`](Self::take_gui_closed).
    gui_closed: AtomicBool,
    /// Companion to `gui_closed`: whether the plugin also destroyed the GUI.
    gui_destroyed: AtomicBool,
    /// Set by [`HostGuiImpl::request_resize`]; drained by
    /// [`take_resize_request`](Self::take_resize_request). Only meaningful for
    /// an **embedded** GUI (a floating one owns its own OS window and resizes
    /// it directly) — `ClapEditor::pump` discards it when there is no parent
    /// `PluginWindow` to resize.
    resize_pending: AtomicBool,
    /// Requested embedded-window width, paired with `resize_pending`.
    resize_width: AtomicU32,
    /// Requested embedded-window height, paired with `resize_pending`.
    resize_height: AtomicU32,
    /// The plugin's `gui` extension, resolved once at init (`None` if absent).
    gui: OnceLock<Option<PluginGui>>,
    /// The plugin's `timer` extension, resolved once at init.
    timer: OnceLock<Option<PluginTimer>>,
    /// The plugin's `state` extension, or `None` if it does not implement it —
    /// such a plugin simply has no persisted preset.
    state: OnceLock<Option<PluginState>>,
    /// The plugin's `params` extension, or `None` if it does not implement it —
    /// the knobs have nothing to drive then.
    params: OnceLock<Option<PluginParams>>,
    /// Lets the plugin wake the reactive UI to have its callback / close
    /// serviced, so `update` no longer polls at a fixed rate. Never the egui
    /// context directly: `request_callback`, `request_resize` and `closed` may
    /// come from any thread, the audio thread included.
    wake: Arc<UiWake>,
}

impl ClapHostShared {
    /// A fresh shared handler; the extension `OnceLock`s fill in at plugin
    /// init.
    pub(crate) fn new(wake: Arc<UiWake>) -> Self {
        Self {
            callback_requested: AtomicBool::new(false),
            process_requested: AtomicBool::new(false),
            gui_closed: AtomicBool::new(false),
            gui_destroyed: AtomicBool::new(false),
            resize_pending: AtomicBool::new(false),
            resize_width: AtomicU32::new(0),
            resize_height: AtomicU32::new(0),
            gui: OnceLock::new(),
            timer: OnceLock::new(),
            state: OnceLock::new(),
            params: OnceLock::new(),
            wake,
        }
    }

    /// Wakes the eframe UI so `ClapEditor::pump` runs on the next frame. Never
    /// blocks, so any host callback may call it.
    fn wake_ui(&self) {
        self.wake.request();
    }

    /// Atomically clears and returns the "plugin asked for a main-thread
    /// callback" flag.
    pub(crate) fn take_callback_request(&self) -> bool {
        self.callback_requested.swap(false, Ordering::Acquire)
    }

    /// Atomically clears and returns the "plugin asked to be processed" flag.
    /// Called for every voice on every block, so the common not-requested
    /// case is a plain load rather than a read-modify-write.
    pub(crate) fn take_process_request(&self) -> bool {
        self.process_requested.load(Ordering::Relaxed)
            && self.process_requested.swap(false, Ordering::Acquire)
    }

    /// Atomically clears and returns the "floating window was closed" signal.
    /// `Some(was_destroyed)` when a close happened since the last call.
    pub(crate) fn take_gui_closed(&self) -> Option<bool> {
        if self.gui_closed.swap(false, Ordering::Acquire) {
            Some(self.gui_destroyed.swap(false, Ordering::Acquire))
        } else {
            None
        }
    }

    /// Atomically clears and returns a pending "resize the embedded parent
    /// window" request, if the plugin made one since the last call.
    pub(crate) fn take_resize_request(&self) -> Option<(u32, u32)> {
        if self.resize_pending.swap(false, Ordering::Acquire) {
            Some((
                self.resize_width.load(Ordering::Acquire),
                self.resize_height.load(Ordering::Acquire),
            ))
        } else {
            None
        }
    }

    /// The plugin's `gui` extension, if it has one.
    pub(crate) fn plugin_gui(&self) -> Option<PluginGui> {
        self.gui.get().copied().flatten()
    }

    /// The plugin's `timer` extension, if it has one.
    pub(crate) fn plugin_timer(&self) -> Option<PluginTimer> {
        self.timer.get().copied().flatten()
    }

    /// The plugin's `state` extension, if it has one (preset persistence).
    pub(crate) fn plugin_state(&self) -> Option<PluginState> {
        self.state.get().copied().flatten()
    }

    /// The plugin's `params` extension, if it has one.
    pub(crate) fn plugin_params(&self) -> Option<PluginParams> {
        self.params.get().copied().flatten()
    }
}

impl<'a> SharedHandler<'a> for ClapHostShared {
    fn initializing(&self, instance: InitializingPluginHandle<'a>) {
        let _ = self.gui.set(instance.get_extension());
        let _ = self.timer.set(instance.get_extension());
        let _ = self.state.set(instance.get_extension());
        let _ = self.params.set(instance.get_extension());
    }

    // A restart request (deactivate + reactivate) is not acted on.
    fn request_restart(&self) {}
    fn request_process(&self) {
        self.process_requested.store(true, Ordering::Release);
    }
    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
        self.wake_ui();
    }
}

impl HostLogImpl for ClapHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        if severity == LogSeverity::Debug {
            return;
        }
        debug!("clap plugin [{severity}]: {message}");
    }
}

impl HostParamsImplShared for ClapHostShared {
    /// The plugin has parameter changes for the host: see the module docs.
    fn request_flush(&self) {
        self.request_process();
    }
}

impl HostThreadCheckImpl for ClapHostShared {
    fn is_main_thread(&self) -> bool {
        MainThreadMarker::new().is_some()
    }

    fn is_audio_thread(&self) -> bool {
        is_audio_thread()
    }
}

impl HostGuiImpl for ClapHostShared {
    fn resize_hints_changed(&self) {}

    // Stashes the requested size for `ClapEditor::pump` (main thread) to apply
    // to our parent `NSWindow` via `PluginWindow::set_content_size` — e.g. when
    // the user zooms an embedded editor and the plugin asks its parent to
    // grow/shrink to match. A no-op in practice for a floating GUI (the plugin
    // owns its own OS window there and resizes it directly without going
    // through the host), but accepted the same way regardless — `pump`
    // discards the request when there's no `PluginWindow` to resize.
    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        self.resize_width.store(new_size.width, Ordering::Release);
        self.resize_height.store(new_size.height, Ordering::Release);
        self.resize_pending.store(true, Ordering::Release);
        self.wake_ui();
        Ok(())
    }
    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, was_destroyed: bool) {
        self.gui_destroyed.store(was_destroyed, Ordering::Release);
        self.gui_closed.store(true, Ordering::Release);
        self.wake_ui();
    }
}

/// `[main-thread]` host state: the timer registry and the rescan flag. The
/// `'a` borrow of [`ClapHostShared`] keeps the type tied to the instance
/// lifetime (and satisfies the `MainThreadHandler<'a>` bound).
pub(crate) struct ClapHostMainThread<'a> {
    /// Borrow of the shared handler (ties the lifetime to the instance).
    shared: &'a ClapHostShared,
    /// The plugin's registered CLAP timers. `RefCell` because clack hands the
    /// main-thread handler out by shared reference only (`access_handler`,
    /// and the `HostTimerImpl` callbacks take `&self`); the type is
    /// main-thread-only, so this never contends and is not a lock on any
    /// audio or render path.
    timers: RefCell<Timers>,
    /// Set by `rescan` when the plugin's values changed without events (a
    /// preset it loaded); drained by
    /// [`take_values_rescan`](Self::take_values_rescan). The plugin is inside
    /// a call of its own then, so it isn't read back from there.
    values_rescan: Cell<bool>,
    /// Set by `mark_dirty`; drained by
    /// [`take_state_dirty`](Self::take_state_dirty), as `values_rescan` is.
    state_dirty: Cell<bool>,
}

impl<'a> ClapHostMainThread<'a> {
    /// A main-thread handler borrowing `shared`, with no timers yet.
    pub(crate) fn new(shared: &'a ClapHostShared) -> Self {
        Self {
            shared,
            timers: RefCell::new(Timers::default()),
            values_rescan: Cell::new(false),
            state_dirty: Cell::new(false),
        }
    }

    /// Returns the ids of every registered timer that is due to fire at `now`.
    pub(crate) fn due_timers(&self, now: Instant) -> Vec<TimerId> {
        self.timers.borrow_mut().tick(now)
    }

    /// Shortest registered timer period, or `None` if the plugin registered no
    /// timers — the cadence the UI must repaint at to service the plugin editor.
    pub(crate) fn min_timer_period(&self) -> Option<Duration> {
        self.timers.borrow().entries.iter().map(|e| e.period).min()
    }

    /// Clears and returns whether the plugin asked for its values to be read
    /// again since the last call.
    pub(crate) fn take_values_rescan(&self) -> bool {
        self.values_rescan.take()
    }

    /// Clears and returns whether the plugin marked its state dirty since
    /// the last call.
    pub(crate) fn take_state_dirty(&self) -> bool {
        self.state_dirty.take()
    }
}

impl<'a> MainThreadHandler<'a> for ClapHostMainThread<'a> {}

impl<'a> HostStateImpl for ClapHostMainThread<'a> {
    /// The plugin says its state changed since the last save/load. Marked
    /// for [`ClapEditor`](super::ClapEditor) to report as
    /// [`ParameterGesture::ValuesChanged`](crate::core::plugin_host::editor::ParameterGesture::ValuesChanged),
    /// which the unsaved prompt counts (`060-persistence.md` § Unsaved
    /// changes), and the UI woken to read it, once until it is read. The
    /// state itself is captured fresh on every save.
    fn mark_dirty(&self) {
        if !self.state_dirty.replace(true) {
            debug!("clap host: plugin marked its state dirty");
            self.shared.wake_ui();
        }
    }
}

impl<'a> HostParamsImplMainThread for ClapHostMainThread<'a> {
    /// Marks the values to be read again, and wakes the UI so a knob whose
    /// parameter a preset moved follows at once. A changed name or text is
    /// left: names are read once at load, as VST3's are.
    fn rescan(&self, flags: ParamRescanFlags) {
        if flags.intersects(ParamRescanFlags::VALUES | ParamRescanFlags::ALL) {
            self.values_rescan.set(true);
            self.shared.wake_ui();
        }
    }

    /// Stev holds a parameter's data as knobs and clip moves, which stay: a
    /// parameter that's gone leaves its knob with no name, and the user
    /// decides what to do with it.
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl<'a> HostTimerImpl for ClapHostMainThread<'a> {
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let id = self.timers.borrow_mut().register(period_ms);
        debug!("clap host: plugin registered timer {id} @ {period_ms}ms");
        // `register_timer` runs during `gui.create`/`show`, i.e. inside the same
        // `update` that opened the editor — wake the UI so the next frame picks
        // up the new repaint cadence.
        self.shared.wake_ui();
        Ok(id)
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        self.timers.borrow_mut().unregister(timer_id);
        self.shared.wake_ui();
        Ok(())
    }
}

/// Minimal port of the clack `cpal` example's timer helper: tracks registered
/// CLAP timers and, on each [`tick`](Self::tick), reports which are due.
#[derive(Default)]
struct Timers {
    /// Registered timers.
    entries: Vec<TimerEntry>,
    /// Next [`TimerId`] to hand out.
    next_id: u32,
}

/// One registered CLAP timer.
struct TimerEntry {
    /// Its id.
    id: TimerId,
    /// Firing period (clamped to a sane minimum).
    period: Duration,
    /// When it last fired, `None` until the first.
    last_fired: Option<Instant>,
}

impl Timers {
    /// Registers a new timer and returns its id.
    fn register(&mut self, period_ms: u32) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id += 1;
        // Clamp very short periods — CLAP hosts are only expected to guarantee
        // ~30 Hz.
        let period = Duration::from_millis(u64::from(period_ms).max(16));
        self.entries.push(TimerEntry {
            id,
            period,
            last_fired: None,
        });
        id
    }

    /// Removes the timer with this id.
    fn unregister(&mut self, id: TimerId) {
        self.entries.retain(|e| e.id != id);
    }

    /// Ids of every timer whose period has elapsed by `now`, marking them
    /// fired.
    fn tick(&mut self, now: Instant) -> Vec<TimerId> {
        let mut due = Vec::new();
        for entry in &mut self.entries {
            let fire = match entry.last_fired {
                None => true,
                Some(last) => now.duration_since(last) >= entry.period,
            };
            if fire {
                entry.last_fired = Some(now);
                due.push(entry.id);
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared handler and the wake it raises.
    fn shared_and_wake() -> (ClapHostShared, Arc<UiWake>) {
        let wake = Arc::new(UiWake::new(Arc::new(OnceLock::new())));
        (ClapHostShared::new(Arc::clone(&wake)), wake)
    }

    #[test]
    fn a_dirty_state_is_taken_exactly_once_and_wakes_the_ui() {
        let (shared, wake) = shared_and_wake();
        let main = ClapHostMainThread::new(&shared);
        assert!(!main.take_state_dirty());
        main.mark_dirty();
        assert!(wake.take());
        assert!(main.take_state_dirty());
        assert!(!main.take_state_dirty());
    }

    #[test]
    fn a_callback_request_raises_the_ui_wake() {
        let (shared, wake) = shared_and_wake();
        shared.request_callback();
        assert!(wake.take());
        assert!(shared.take_callback_request());
    }

    #[test]
    fn a_resize_request_raises_the_ui_wake() {
        let (shared, wake) = shared_and_wake();
        shared
            .request_resize(GuiSize {
                width: 640,
                height: 480,
            })
            .unwrap();
        assert!(wake.take());
        assert_eq!(shared.take_resize_request(), Some((640, 480)));
    }

    #[test]
    fn a_closed_gui_raises_the_ui_wake() {
        let (shared, wake) = shared_and_wake();
        shared.closed(true);
        assert!(wake.take());
        assert_eq!(shared.take_gui_closed(), Some(true));
    }

    #[test]
    fn a_flush_request_wakes_the_voice_as_a_process_request_does() {
        let (shared, _wake) = shared_and_wake();
        shared.request_flush();
        assert!(shared.take_process_request());
        assert!(!shared.take_process_request());
    }

    #[test]
    fn a_process_request_is_taken_exactly_once() {
        let (shared, _wake) = shared_and_wake();
        assert!(!shared.take_process_request());
        shared.request_process();
        assert!(shared.take_process_request());
        assert!(!shared.take_process_request());
    }

    #[test]
    fn timer_fires_once_then_waits_for_its_period() {
        let mut timers = Timers::default();
        let id = timers.register(50);
        let t0 = Instant::now();

        // First tick always fires (last_fired is None).
        assert_eq!(timers.tick(t0), vec![id]);
        // Too soon — not due yet.
        assert!(timers.tick(t0 + Duration::from_millis(10)).is_empty());
        // Period elapsed — fires again.
        assert_eq!(timers.tick(t0 + Duration::from_millis(60)), vec![id]);
    }

    #[test]
    fn unregister_stops_a_timer() {
        let mut timers = Timers::default();
        let id = timers.register(10);
        timers.unregister(id);
        assert!(timers.tick(Instant::now()).is_empty());
    }

    #[test]
    fn short_periods_are_clamped() {
        let mut timers = Timers::default();
        timers.register(1);
        assert_eq!(timers.entries[0].period, Duration::from_millis(16));
    }
}
