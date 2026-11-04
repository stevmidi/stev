//! Waking the reactive UI from the audio thread.
//!
//! `egui::Context::request_repaint` takes a lock, which the audio thread may
//! not. So a voice with news for the UI raises a flag here and unparks the
//! `"plugin-host"` thread, which makes the call for it: an atomic swap, and a
//! signal only when the flag was down. A CLAP plugin's editor turn reaches the
//! host only through `process` (`400-parameter-knobs.md`), so without this the
//! knob wouldn't follow while nothing else repaints.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;

/// See the module docs.
#[derive(Default)]
pub(crate) struct UiWake {
    /// Raised by [`request`](Self::request), lowered by [`take`](Self::take).
    wanted: AtomicBool,
    /// The `"plugin-host"` thread, to unpark.
    thread: OnceLock<Thread>,
}

impl UiWake {
    /// (`"plugin-host"` thread) Registers itself for `unpark`.
    pub(super) fn register_thread(&self) {
        let _ = self.thread.set(std::thread::current());
    }

    /// (any thread, the audio thread included) Asks for a repaint. Never
    /// blocks.
    pub(crate) fn request(&self) {
        if !self.wanted.swap(true, Ordering::AcqRel)
            && let Some(thread) = self.thread.get()
        {
            thread.unpark();
        }
    }

    /// (`"plugin-host"` thread) Clears and returns whether a repaint was
    /// asked for since the last call.
    pub(super) fn take(&self) -> bool {
        self.wanted.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_taken_exactly_once() {
        let wake = UiWake::default();
        assert!(!wake.take());
        wake.request();
        wake.request();
        assert!(wake.take());
        assert!(!wake.take());
    }
}
