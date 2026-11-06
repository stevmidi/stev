//! Waking the reactive UI from the audio thread.
//!
//! `egui::Context::request_repaint` takes a lock, which the audio thread may
//! not. So a voice with news for the UI raises a flag here and unparks the
//! `"plugin-host"` thread, which makes the call for it: an atomic swap, and a
//! signal only when the flag was down. A CLAP plugin's editor turn reaches the
//! host only through `process` (`400-parameter-knobs.md`), so without this the
//! knob wouldn't follow while nothing else repaints. CLAP's host callbacks
//! wake the UI this way too, as a plugin may make them from any thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::Thread;

/// See the module docs.
pub(crate) struct UiWake {
    /// Raised by [`request`](Self::request), lowered by [`take`](Self::take).
    wanted: AtomicBool,
    /// The `"plugin-host"` thread, to unpark.
    thread: OnceLock<Thread>,
    /// The app's egui context, once it exists.
    repaint: Arc<OnceLock<egui::Context>>,
}

impl UiWake {
    /// A wake for the UI of `repaint`.
    pub(super) fn new(repaint: Arc<OnceLock<egui::Context>>) -> Self {
        Self {
            wanted: AtomicBool::new(false),
            thread: OnceLock::new(),
            repaint,
        }
    }

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

    /// (`"plugin-host"` thread) Makes the repaint, if one was asked for.
    pub(super) fn service(&self) {
        if self.take()
            && let Some(ctx) = self.repaint.get()
        {
            ctx.request_repaint();
        }
    }

    /// Clears and returns whether a repaint was asked for since the last
    /// call.
    pub(super) fn take(&self) -> bool {
        self.wanted.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_taken_exactly_once() {
        let wake = UiWake::new(Arc::new(OnceLock::new()));
        assert!(!wake.take());
        wake.request();
        wake.request();
        assert!(wake.take());
        assert!(!wake.take());
    }
}
