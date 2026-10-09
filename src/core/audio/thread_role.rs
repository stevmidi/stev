//! Which threads count as "the audio thread".
//!
//! CLAP's `thread-check` host extension lets a plugin ask whether it is being
//! called on the audio thread. In this engine that is not one thread: the
//! `cpal` callback thread renders, and so does every
//! [`WorkerPool`](super::WorkerPool) worker (plugin `process` calls are spread
//! across them). Each of those threads marks itself with
//! [`enter_render_thread`]; [`is_audio_thread`] reads the mark back.
//!
//! The mark is a const-initialised thread-local `Cell`, so setting and reading
//! it is a plain TLS load/store — no allocation, no lock, safe to call every
//! callback. The `cpal` thread isn't ours to hook at start-up, so it is marked
//! on entry to each callback.

use std::cell::Cell;

use super::denormals::flush_denormals_to_zero;

thread_local! {
    /// Whether the current thread renders audio.
    static IS_AUDIO_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Sets up the **calling thread** to render audio: flushes denormals (see
/// [`denormals`](super::denormals)) and marks it as an audio thread for
/// [`is_audio_thread`]. Both are per-thread, so every thread that runs DSP —
/// the `cpal` callback and each [`WorkerPool`](super::WorkerPool) worker —
/// calls this. Idempotent and cheap enough to call per audio callback.
pub(crate) fn enter_render_thread() {
    flush_denormals_to_zero();
    mark_audio_thread();
}

/// Marks the **calling thread** as an audio thread.
fn mark_audio_thread() {
    IS_AUDIO_THREAD.with(|flag| flag.set(true));
}

/// Whether the calling thread has been marked by [`enter_render_thread`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn is_audio_thread() -> bool {
    IS_AUDIO_THREAD.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn unmarked_thread_is_not_an_audio_thread() {
        let marked = thread::spawn(is_audio_thread).join().unwrap();
        assert!(!marked);
    }

    #[test]
    fn mark_is_per_thread() {
        let marked = thread::spawn(|| {
            mark_audio_thread();
            is_audio_thread()
        })
        .join()
        .unwrap();
        assert!(marked);
        // A fresh thread doesn't inherit the other thread's mark.
        let other = thread::spawn(is_audio_thread).join().unwrap();
        assert!(!other);
    }
}
