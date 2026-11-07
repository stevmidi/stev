//! A temporary probe in the `"sequencer"` loop, for `420-large-projects.md`:
//! how late each tick is handled, and how long each arm of the loop holds the
//! thread. Once a second, while ticks arrive, it logs the worst of each to
//! `stev.log`, so a run on the stress project with ⌘S, edits and recovery
//! writes shows what passes the one-tick budget and where. It goes once the
//! design's last phase has been measured with it.

use std::time::{Duration, Instant};

use log::info;

/// How often the probe logs.
const WINDOW: Duration = Duration::from_secs(1);

/// The one-tick budget (`420-large-projects.md` decision 2).
const BUDGET: Duration = Duration::from_millis(1);

/// The worst seen in the current window. See the module docs.
pub(super) struct TickProbe {
    /// When the current window began.
    window_start: Instant,
    /// Ticks handled in the window.
    ticks: u32,
    /// The latest a tick's work finished after its intended instant.
    worst_late: Duration,
    /// Ticks whose work finished more than [`BUDGET`] after their instant.
    late_ticks: u32,
    /// The longest one arm held the thread, and which arm.
    worst_arm: (Duration, &'static str),
    /// Arms that held the thread for more than [`BUDGET`].
    long_arms: u32,
}

impl TickProbe {
    /// A probe whose first window starts now.
    pub(super) fn new() -> Self {
        Self {
            window_start: Instant::now(),
            ticks: 0,
            worst_late: Duration::ZERO,
            late_ticks: 0,
            worst_arm: (Duration::ZERO, ""),
            long_arms: 0,
        }
    }

    /// The tick meant for `at` has been handled.
    pub(super) fn tick_handled(&mut self, at: Instant) {
        let late = Instant::now().saturating_duration_since(at);
        self.ticks += 1;
        self.late_ticks += u32::from(late > BUDGET);
        self.worst_late = self.worst_late.max(late);
    }

    /// The arm `name`, begun at `started`, is done. Logs the window when it
    /// has run its second and held a tick.
    pub(super) fn arm_done(&mut self, name: &'static str, started: Instant) {
        let now = Instant::now();
        let held = now.saturating_duration_since(started);
        self.long_arms += u32::from(held > BUDGET);
        if held > self.worst_arm.0 {
            self.worst_arm = (held, name);
        }
        if now.saturating_duration_since(self.window_start) < WINDOW {
            return;
        }
        if self.ticks > 0 {
            info!(
                "tick probe: {} ticks, worst late {:.2} ms, {} past 1 ms; longest arm {} {:.2} ms, {} past 1 ms",
                self.ticks,
                ms(self.worst_late),
                self.late_ticks,
                self.worst_arm.1,
                ms(self.worst_arm.0),
                self.long_arms,
            );
        }
        *self = Self::new();
    }
}

/// `time` in milliseconds.
fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}
