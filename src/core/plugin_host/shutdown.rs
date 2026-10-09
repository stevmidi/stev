//! Cross-thread shutdown coordination for the plugin host, shared by every
//! plugin format.
//!
//! On macOS `eframe::run_native` never returns (AppKit's `terminate:` calls
//! `exit()` after `App::on_exit`), so none of our `Drop`s run in order. Without
//! this, the audio-engine callback keeps calling into a plugin while `exit()`
//! runs the plugin bundle's static destructors — a data race that segfaults.
//! `Display::on_exit` sets [`requested`](HostShutdown::requested) and waits for
//! any in-flight process call to return before the plugins are torn down. The
//! [`InstrumentMixer`](super::mixer::InstrumentMixer) source and its
//! still-loaded voices are then left to leak with the parked engine thread —
//! deliberately, since dropping them would re-enter the very plugin code
//! `exit()` is unloading.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

/// How long [`HostShutdown::request_and_wait`] will block for an in-flight
/// process call to return before giving up and letting teardown proceed. A
/// hard cap: a wedged audio callback must not hang app exit.
const WAIT_TIMEOUT: Duration = Duration::from_millis(500);

/// Poll interval while waiting out an in-flight process call.
const WAIT_POLL: Duration = Duration::from_millis(2);

/// See the module docs.
#[derive(Default)]
pub(crate) struct HostShutdown {
    /// Set on app exit. The mixer source then stops calling into any plugin,
    /// and the `"plugin-host"` reclaim thread exits.
    requested: AtomicBool,
    /// Held `true` by the mixer while it is inside `render_into` calling into a
    /// plugin, so shutdown can wait for any in-flight call to return.
    in_process: AtomicBool,
    /// The `"plugin-host"` reclaim thread, so shutdown can wake it from its
    /// park loop instead of waiting out the park timeout.
    thread: OnceLock<Thread>,
}

impl HostShutdown {
    /// (audio thread) Whether shutdown has begun — skip the plugin if so.
    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// (audio thread) Marks whether the mixer is currently in a process call.
    pub(crate) fn set_in_process(&self, v: bool) {
        self.in_process.store(v, Ordering::SeqCst);
    }

    /// (`"plugin-host"` reclaim thread) Registers itself for `unpark`.
    pub(crate) fn register_thread(&self) {
        let _ = self.thread.set(std::thread::current());
    }

    /// (main thread, `on_exit`) Requests shutdown and blocks — with a hard
    /// timeout — until no process call is in flight.
    pub(crate) fn request_and_wait(&self) {
        self.requested.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
        let deadline = Instant::now() + WAIT_TIMEOUT;
        while self.in_process.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(WAIT_POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_neither_requested_nor_in_process() {
        let s = HostShutdown::default();
        assert!(!s.is_requested());
        s.request_and_wait();
        assert!(s.is_requested());
    }

    #[test]
    fn request_and_wait_returns_once_the_in_flight_call_clears() {
        let s = HostShutdown::default();
        s.set_in_process(true);
        s.set_in_process(false);
        let started = Instant::now();
        s.request_and_wait();
        // Nothing in flight, so it must not sit out the timeout.
        assert!(started.elapsed() < WAIT_TIMEOUT);
    }
}
