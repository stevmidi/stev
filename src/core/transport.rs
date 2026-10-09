//! Playback position, the loop region, and the cursor.
//!
//! [`Transport`] is a `"sequencer"`-thread-local, like `Metronome` — it is
//! never shared, so its non-published state stays plain (`bool`, `i32`). The
//! state the rest of the app needs is mirrored into
//! [`SharedAtomics`](crate::core::shared_atomics::SharedAtomics) through the
//! `Arc<AtomicX>` handles it is constructed with; its `Region` reads and writes
//! `region_start`/`region_end` straight through
//! ([`Region::from_shared`](crate::models::region::Region::from_shared)).
//!
//! [`TransportCommand`]s arrive on their own `select!` arm; [`TransportEvent`]s
//! and [`TickOutcome`] flow the other way, telling the tick pump when playback
//! moved discontinuously and it must re-anchor (re-seek tracks, realign the
//! clock, release notes). The clock realignment is a *phase* move, never an
//! assignment — see `150-clock-position-sync.md`.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use crossbeam_channel::Sender;

use crate::core::clock::ClockCommand;
use crate::core::time::step_to_grid;
use crate::models::region::Region;

/// Something the transport did that the `"sequencer"` tick pump must react to.
/// Sent on `transport_event_rx`; handled a wakeup later, which is why a loop
/// wrap uses [`TickOutcome::Wrapped`] instead (synchronous).
pub(crate) enum TransportEvent {
    /// Playback jumped to this tick — re-seek every track to it.
    PlaybackTickReset(i32),
    /// Playback jumped to this tick *while playing* — re-seek and chase notes
    /// so held notes at the destination sound.
    PlaybackTickResetWithChase(i32),
    /// The transport stopped.
    Stopped,
}

/// What [`Transport::tick`] did on this tick.
pub(crate) enum TickOutcome {
    /// Playback advanced by one tick.
    Advanced,
    /// Playback reached the loop-region end and wrapped back to the carried tick
    /// (`region_start`). The `"sequencer"` thread's tick pump must re-anchor
    /// playback for the new position — re-seek tracks, realign the clock,
    /// release sounding notes — **synchronously, before the next
    /// [`Sequencer::tick`](crate::core::sequencer::Sequencer::tick)**. Routing this
    /// through `transport_event_rx` instead
    /// let a burst of ticks handled in one wakeup run past the region end into
    /// the next clip, leaking its notes into an instrument plugin (a stuck note
    /// every few loops).
    Wrapped(i32),
}

/// The transport-only command stream (`transport_command_rx`). Handled against
/// the thread-local [`Transport`] in the same `select!` loop as
/// [`SequencerCommand`](crate::core::sequencer::SequencerCommand).
pub(crate) enum TransportCommand {
    /// Set either region bound; `None` leaves that bound where it is.
    SetRegion {
        /// New region start tick, or `None` to keep it.
        start: Option<i32>,
        /// New region end tick, or `None` to keep it.
        end: Option<i32>,
    },
    /// Move the cursor by a signed tick amount, snapping onto the grid implied
    /// by the step size (see [`Transport::move_cursor`]).
    MoveCursor {
        /// Signed tick delta / step size.
        ticks: i32,
    },
    /// Sets the region to the given bounds — a selected clip's span or an arranger time
    /// selection — mirroring Ableton's loop-region toggle: if the region already matches these
    /// bounds, toggles looping on/off instead of re-applying an identical region. A genuinely
    /// new region always re-enables looping.
    SetRegionOrToggleLoop {
        /// Target region start tick.
        start: i32,
        /// Target region end tick.
        end: i32,
    },
    /// Loops over `[start, end)`: sets the region and turns looping on,
    /// whatever it was — unlike `SetRegionOrToggleLoop`, never toggles it
    /// off. The one caller: the stopped `/` that makes a project's first
    /// clip loops over it, so its end can be judged against the wrap before
    /// Enter sets the tempo (`220-capture-without-pending-view.md`).
    LoopOver {
        /// Region start tick.
        start: i32,
        /// Region end tick.
        end: i32,
    },
    /// Flips `loop_enabled` without touching the region — `⌘/Ctrl+L` with no
    /// time selection. The region band stays where it is (dimmed while looping
    /// is off) so the next press turns it back on in place.
    ToggleLoop,
    /// Sets the transport cursor to an absolute tick and re-syncs clip
    /// selection to whatever clip (if any) now sits under the cursor.
    /// Used for click-to-place-cursor in the arranger view.
    SetCursorAndSelectClip {
        /// Absolute target tick.
        tick: i32,
    },
    /// Stop, reposition playback to `tick`, start again, without touching
    /// the cursor. The clip view's `⌥Space`, which starts
    /// from the clip cursor while the arranger cursor stays put.
    PlayFromTick {
        /// Absolute (arrangement) tick to start from.
        tick: i32,
    },
    /// Stop playback.
    Stop,
    /// Start if stopped, stop if playing.
    TogglePlayback,
    /// Mute / unmute the built-in metronome click.
    ToggleMetronomeMute,
}

/// Owns playback position, the loop region and the cursor for the
/// `"sequencer"` thread. See the module docs.
pub(crate) struct Transport {
    // --- Playback and cursor state ---
    /// Playback position, shared with the rest of the app.
    playback_tick: Arc<AtomicI32>,
    /// Edit-cursor / play-from position, shared.
    cursor_tick: Arc<AtomicI32>,
    /// Whether playback is running, shared.
    running: Arc<AtomicBool>,
    /// The loop region — a view onto the shared `region_start`/`region_end`
    /// atomics, not an owned copy.
    region: Region,
    /// Whether playback wraps at the region end, shared.
    loop_enabled: Arc<AtomicBool>,
    /// Suspends region loop-wrap while the arranger performance lane is
    /// actively holding a bar-jump (live or replayed) — `Transport` only
    /// ever lives as a sequencer-thread-local, so no cross-thread read is
    /// needed and this stays a plain `bool`, unlike `loop_enabled`.
    loop_wrap_suspended: bool,

    // --- Communication ---
    /// Discontinuity notifications to the `"sequencer"` tick pump.
    transport_event_tx: Sender<TransportEvent>,
    /// Phase-realignment requests to the `"clock"` thread.
    clock_command_tx: Sender<ClockCommand>,
}

impl Transport {
    // --- Constructor ---
    /// Wires the transport to the shared atomics it mirrors state into. The
    /// `region_start`/`region_end` pair becomes its `Region`; the rest are held
    /// as-is. Parameter order mirrors the field grouping (channels, then the
    /// atomics) — see `080-conventions.md`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        transport_event_tx: Sender<TransportEvent>,
        clock_command_tx: Sender<ClockCommand>,
        playback_tick: Arc<AtomicI32>,
        cursor_tick: Arc<AtomicI32>,
        running: Arc<AtomicBool>,
        region_start: Arc<AtomicI32>,
        region_end: Arc<AtomicI32>,
        loop_enabled: Arc<AtomicBool>,
    ) -> Self {
        Transport {
            playback_tick,
            cursor_tick,
            running,
            region: Region::from_shared(region_start, region_end),
            loop_enabled,
            loop_wrap_suspended: false,
            transport_event_tx,
            clock_command_tx,
        }
    }

    // --- Playback accessors and transport control ---
    /// Whether the transport is currently playing.
    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Advances `playback_tick` by one. On a loop wrap it stores `region_start`
    /// and returns [`TickOutcome::Wrapped`] so the caller can re-anchor playback
    /// synchronously — see that variant's doc for why this is not a
    /// [`TransportEvent`]. The wrap path deliberately does **not** go through
    /// `set_playback_tick` (no `PlaybackTickReset` is sent).
    #[must_use]
    pub(crate) fn tick(&mut self) -> TickOutcome {
        let start = self.region.start();
        let end = self.region.end();

        let current = self.playback_tick.load(Ordering::Relaxed);
        let next = current + 1;

        // Loop wrap applies only while looping is enabled and playback is currently
        // inside the active region window. When starting from a cursor outside the
        // region, keep advancing from cursor instead of snapping to region start.
        if self.is_loop_enabled()
            && !self.loop_wrap_suspended
            && current >= start
            && current < end
            && next >= end
        {
            self.playback_tick.store(start, Ordering::Relaxed);
            TickOutcome::Wrapped(start)
        } else {
            self.playback_tick.store(next, Ordering::Relaxed);
            TickOutcome::Advanced
        }
    }

    /// Begins playback. Idempotent; sends no [`TransportEvent`].
    pub(crate) fn start(&mut self) {
        self.running.store(true, Ordering::Relaxed);
    }

    /// Halts playback and sends [`TransportEvent::Stopped`].
    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.transport_event_tx.send(TransportEvent::Stopped);
    }

    /// Jumps playback to the cursor and chases notes there, staying in
    /// whatever running state it was in.
    pub(crate) fn restart_from_cursor(&mut self) {
        let tick = self.cursor_tick.load(Ordering::Relaxed);
        dprintln!("Restarting from cursor at tick: {}", tick);
        self.jump_and_chase(tick);
    }

    /// Jumps playback to an arbitrary absolute tick while continuing to
    /// play — the same "jump while playing" behavior `restart_from_cursor`
    /// uses, generalized to a caller-supplied target instead of always
    /// reading `cursor_tick`. Used by the arranger performance lane to jump
    /// to a bar position on a NoteOn trigger.
    pub(crate) fn jump_and_chase(&mut self, tick: i32) {
        self.playback_tick.store(tick, Ordering::Relaxed);
        self.transport_event_tx
            .send(TransportEvent::PlaybackTickResetWithChase(tick))
            .ok();
    }

    /// Suspends region loop-wrap for the duration of an arranger
    /// performance-lane bar-jump (live or replayed) so the loop boundary
    /// doesn't fight the lane's own hold.
    pub(crate) fn suspend_loop_wrap(&mut self) {
        self.loop_wrap_suspended = true;
    }

    /// Re-enables region loop-wrap after a performance-lane bar-jump releases.
    pub(crate) fn resume_loop_wrap(&mut self) {
        self.loop_wrap_suspended = false;
    }

    /// Hands the `"clock"` thread what it needs to realign its free-running
    /// counter with playback inside the current loop region. Sent on any
    /// playback discontinuity, running or not — [`TransportCommand::PlayFromTick`]
    /// repositions while briefly stopped, and gating that away would leave the
    /// clock in a stale phase for the whole of the next playback.
    ///
    /// A loop wrap goes through here too and costs a few ticks at most: the
    /// clock is only ever moved onto playback's *phase*, keeping the loop index
    /// it free-ran to (see `Clock::aligned_tick`).
    pub(crate) fn align_clock_with_playback(&self) {
        self.clock_command_tx
            .send(ClockCommand::AlignToPlayback {
                playback_tick: self.playback_tick.load(Ordering::Relaxed),
                region_start: self.region.start(),
                region_length: self.region.end() - self.region.start(),
            })
            .ok();
    }

    /// Same, for a region edit that left playback where it is — the phase space
    /// changed under it, so the clock may need remapping into the new one.
    ///
    /// Gated on the transport running: while stopped, `playback_tick` is parked
    /// at the cursor while the clock keeps free-running as the coordinate live
    /// capture and the metronome are counting in. Aligning to a parked playback
    /// position would drag that coordinate backwards mid-phrase.
    pub(crate) fn align_clock_with_playback_if_running(&self) {
        if self.is_running() {
            self.align_clock_with_playback();
        }
    }

    /// Sets the cursor to an absolute tick, clamped to `>= 0`.
    pub(crate) fn set_cursor_tick(&mut self, value: i32) {
        self.cursor_tick.store(value.max(0), Ordering::Relaxed);
    }

    /// Moves the cursor by `ticks` (a signed amount), landing on the next grid
    /// multiple of `|ticks|` when the cursor is currently off-grid — so
    /// repeated presses walk a clean grid rather than an offset one
    /// ([`step_to_grid`], as the clip cursor steps).
    pub(crate) fn move_cursor(&mut self, ticks: i32) {
        if ticks == 0 {
            return;
        }
        let current = self.cursor_tick.load(Ordering::Relaxed);
        self.set_cursor_tick(step_to_grid(current, ticks.abs(), ticks.signum()));
    }

    /// Snaps playback back to the cursor (sends `PlaybackTickReset`).
    pub(crate) fn reset_playback_to_cursor(&mut self) {
        self.set_playback_tick(self.cursor_tick.load(Ordering::Relaxed));
    }

    // --- Loop state ---
    /// Whether playback wraps at the region end.
    pub(crate) fn is_loop_enabled(&self) -> bool {
        self.loop_enabled.load(Ordering::Relaxed)
    }

    /// Sets the loop-enabled flag.
    pub(crate) fn set_loop_enabled(&mut self, value: bool) {
        self.loop_enabled.store(value, Ordering::Relaxed);
    }

    /// Flips the loop-enabled flag.
    pub(crate) fn toggle_loop_enabled(&mut self) {
        self.loop_enabled.fetch_xor(true, Ordering::Relaxed);
    }

    // --- Region accessors and management ---
    /// Mutable access to the loop [`Region`] (which writes straight through to
    /// the shared `region_start`/`region_end` atomics).
    pub(crate) fn region_mut(&mut self) -> &mut Region {
        &mut self.region
    }

    /// [`TransportCommand::LoopOver`]: region to `[start, end)`, looping on,
    /// and the clock re-aligned if playing.
    pub(crate) fn loop_over(&mut self, start: i32, end: i32) {
        self.region.set_region(Some(start), Some(end));
        self.set_loop_enabled(true);
        self.align_clock_with_playback_if_running();
    }

    /// Sets the region to `start`/`end` — a clip span or an arranger time selection — mirroring
    /// Ableton's loop-region toggle behavior: if the region already matches these bounds, toggles
    /// looping instead of re-applying an identical region. Setting a genuinely new region always
    /// re-enables looping. Returns `true` if the region was changed (as opposed to just toggling
    /// the loop state).
    pub(crate) fn set_region_or_toggle_loop(&mut self, start: i32, end: i32) -> bool {
        if self.region.start() == start && self.region.end() == end {
            self.toggle_loop_enabled();
            false
        } else {
            self.region.set_region(Some(start), Some(end));
            self.set_loop_enabled(true);
            true
        }
    }

    // --- Private helpers/setters ---
    /// The one path that repositions playback and announces it with
    /// [`TransportEvent::PlaybackTickReset`]. The loop-wrap path in
    /// [`tick`](Self::tick) deliberately bypasses this.
    fn set_playback_tick(&mut self, new_tick: i32) {
        self.playback_tick.store(new_tick, Ordering::Relaxed);
        self.transport_event_tx
            .send(TransportEvent::PlaybackTickReset(new_tick))
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, unbounded};

    fn make_transport(
        playback: i32,
        cursor: i32,
        region_start: i32,
        region_end: i32,
    ) -> (Transport, Arc<AtomicI32>) {
        let (transport, playback_tick, _clock_command_rx) =
            make_transport_with_clock(playback, cursor, region_start, region_end);
        (transport, playback_tick)
    }

    /// As [`make_transport`], but keeps the clock command receiver alive so a
    /// test can assert on what the transport asked the clock to do.
    fn make_transport_with_clock(
        playback: i32,
        cursor: i32,
        region_start: i32,
        region_end: i32,
    ) -> (Transport, Arc<AtomicI32>, Receiver<ClockCommand>) {
        let (transport_event_tx, _transport_event_rx) = unbounded();
        let (clock_command_tx, clock_command_rx) = unbounded();

        let playback_tick = Arc::new(AtomicI32::new(playback));
        let cursor_tick = Arc::new(AtomicI32::new(cursor));
        let running = Arc::new(AtomicBool::new(false));
        let region_start_atomic = Arc::new(AtomicI32::new(region_start));
        let region_end_atomic = Arc::new(AtomicI32::new(region_end));
        let loop_enabled = Arc::new(AtomicBool::new(true));

        let transport = Transport::new(
            transport_event_tx,
            clock_command_tx,
            playback_tick.clone(),
            cursor_tick,
            running,
            region_start_atomic,
            region_end_atomic,
            loop_enabled,
        );

        (transport, playback_tick, clock_command_rx)
    }

    #[test]
    fn align_clock_with_playback_sends_current_playback_tick_and_region_bounds() {
        let (transport, _playback_tick, clock_command_rx) =
            make_transport_with_clock(1_500, 0, 1_000, 3_000);

        transport.align_clock_with_playback();

        let ClockCommand::AlignToPlayback {
            playback_tick,
            region_start,
            region_length,
        } = clock_command_rx
            .try_recv()
            .expect("expected a clock command");
        assert_eq!(playback_tick, 1_500);
        assert_eq!(region_start, 1_000);
        assert_eq!(region_length, 2_000);
    }

    #[test]
    fn align_clock_with_playback_is_sent_even_while_stopped() {
        // `PlayFromTick` repositions between `stop()` and `start()`; gating
        // this on `is_running` would leave the clock in a stale phase for the
        // whole of the next playback.
        let (transport, _playback_tick, clock_command_rx) =
            make_transport_with_clock(1_500, 0, 1_000, 3_000);
        assert!(!transport.is_running());

        transport.align_clock_with_playback();

        assert!(clock_command_rx.try_recv().is_ok());
    }

    #[test]
    fn align_clock_with_playback_if_running_sends_nothing_while_stopped() {
        // A region edit while stopped must not drag the free-running clock back
        // to a playback tick parked at the cursor — that coordinate is what
        // live capture and the metronome are counting in.
        let (transport, _playback_tick, clock_command_rx) =
            make_transport_with_clock(1_500, 0, 1_000, 3_000);

        transport.align_clock_with_playback_if_running();

        assert!(clock_command_rx.try_recv().is_err());
    }

    #[test]
    fn align_clock_with_playback_if_running_sends_while_running() {
        let (mut transport, _playback_tick, clock_command_rx) =
            make_transport_with_clock(1_500, 0, 1_000, 3_000);
        transport.start();

        transport.align_clock_with_playback_if_running();

        assert!(clock_command_rx.try_recv().is_ok());
    }

    #[test]
    fn tick_wraps_to_region_start_when_playback_is_inside_region() {
        let (mut transport, playback_tick) = make_transport(19, 19, 10, 20);

        assert!(matches!(transport.tick(), TickOutcome::Wrapped(10)));

        assert_eq!(playback_tick.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn tick_from_past_region_does_not_snap_to_region_start() {
        let (mut transport, playback_tick) = make_transport(25, 25, 10, 20);

        assert!(matches!(transport.tick(), TickOutcome::Advanced));

        assert_eq!(playback_tick.load(Ordering::Relaxed), 26);
    }

    #[test]
    fn tick_does_not_wrap_when_loop_disabled() {
        let (mut transport, playback_tick) = make_transport(19, 19, 10, 20);
        transport.set_loop_enabled(false);

        assert!(matches!(transport.tick(), TickOutcome::Advanced));

        assert_eq!(playback_tick.load(Ordering::Relaxed), 20);
    }

    #[test]
    fn move_cursor_walks_the_step_grid_and_stops_at_zero() {
        let (mut transport, _playback_tick) = make_transport(0, 150, 0, 1_000);
        let cursor = |t: &Transport| t.cursor_tick.load(Ordering::Relaxed);

        transport.move_cursor(100); // off-grid → next multiple
        assert_eq!(cursor(&transport), 200);
        transport.move_cursor(100); // on-grid → a full step
        assert_eq!(cursor(&transport), 300);
        transport.set_cursor_tick(250);
        transport.move_cursor(-100); // off-grid → previous multiple
        assert_eq!(cursor(&transport), 200);
        transport.move_cursor(-100); // on-grid → a full step back
        assert_eq!(cursor(&transport), 100);
        transport.move_cursor(-100);
        transport.move_cursor(-100); // clamped at the start
        assert_eq!(cursor(&transport), 0);
    }

    #[test]
    fn a_loop_wrap_does_not_send_a_playback_tick_reset_event() {
        // The wrap is re-anchored synchronously by the tick pump via
        // `TickOutcome::Wrapped`; a `PlaybackTickReset` on the channel would be
        // handled a tick or more later, after `Sequencer::tick` had already run
        // past the region end into the next clip.
        let (transport_event_tx, transport_event_rx) = unbounded();
        let (clock_command_tx, _clock_command_rx) = unbounded();
        let mut transport = Transport::new(
            transport_event_tx,
            clock_command_tx,
            Arc::new(AtomicI32::new(19)),
            Arc::new(AtomicI32::new(19)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicI32::new(10)),
            Arc::new(AtomicI32::new(20)),
            Arc::new(AtomicBool::new(true)),
        );

        assert!(matches!(transport.tick(), TickOutcome::Wrapped(10)));
        assert!(transport_event_rx.try_recv().is_err());
    }

    #[test]
    fn loop_over_sets_the_region_and_never_toggles_looping_off() {
        let (mut transport, _playback_tick) = make_transport(0, 0, 0, 10);
        transport.set_loop_enabled(false);

        transport.loop_over(20, 30);
        assert!(transport.is_loop_enabled());
        assert_eq!(transport.region_mut().start(), 20);
        assert_eq!(transport.region_mut().end(), 30);

        transport.loop_over(20, 30);
        assert!(
            transport.is_loop_enabled(),
            "the same region again stays on"
        );
    }

    #[test]
    fn set_region_or_toggle_loop_new_region_enables_loop_and_changes_region() {
        let (mut transport, _playback_tick) = make_transport(0, 0, 0, 10);
        transport.set_loop_enabled(false);

        let changed = transport.set_region_or_toggle_loop(20, 30);

        assert!(changed);
        assert_eq!(transport.region_mut().start(), 20);
        assert_eq!(transport.region_mut().end(), 30);
        assert!(transport.is_loop_enabled());
    }

    #[test]
    fn set_region_or_toggle_loop_matching_region_toggles_loop_off() {
        let (mut transport, _playback_tick) = make_transport(0, 0, 10, 20);
        assert!(transport.is_loop_enabled());

        let changed = transport.set_region_or_toggle_loop(10, 20);

        assert!(!changed);
        assert!(!transport.is_loop_enabled());
    }

    #[test]
    fn toggle_loop_enabled_flips_flag_and_leaves_region_untouched() {
        let (mut transport, _playback_tick) = make_transport(0, 0, 10, 20);
        assert!(transport.is_loop_enabled());

        transport.toggle_loop_enabled();
        assert!(!transport.is_loop_enabled());
        assert_eq!(transport.region_mut().start(), 10);
        assert_eq!(transport.region_mut().end(), 20);

        transport.toggle_loop_enabled();
        assert!(transport.is_loop_enabled());
        assert_eq!(transport.region_mut().start(), 10);
        assert_eq!(transport.region_mut().end(), 20);
    }

    #[test]
    fn set_region_or_toggle_loop_matching_region_toggles_loop_back_on() {
        let (mut transport, _playback_tick) = make_transport(0, 0, 10, 20);
        transport.set_loop_enabled(false);

        let changed = transport.set_region_or_toggle_loop(10, 20);

        assert!(!changed);
        assert!(transport.is_loop_enabled());
    }
}
