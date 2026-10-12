//! The `"computerkeys"` thread: routes the computer keyboard's notes
//! (`Display`'s note keys, on `computer_keys_rx`) as live MIDI input, through
//! a `LiveInputRouter` of their own — thru, the plugin tap, capture,
//! recording, as a port's input. Off the render thread, so the plugin tap's
//! lock stays off the render path; a thread of its own, so a note never
//! waits behind the `"midiwatcher"`'s port enumeration. Exits when `Display`
//! drops its sender.

use crossbeam_channel::Receiver;

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::core::midi::input::MidiInputForwarder;
use crate::core::midi::message::Midi3;

/// Starts the `"computerkeys"` thread (see the module docs). Each message on
/// `computer_keys_rx` is a note-on / note-off and the `monotonic_nanos` the
/// frame read its key at, which `forwarder` turns into its `InputTicks`.
pub(crate) fn start_computer_keys_thread(
    forwarder: Arc<MidiInputForwarder>,
    computer_keys_rx: Receiver<(Midi3, u64)>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("computerkeys".to_string())
        .spawn(move || {
            let mut router = forwarder.router();
            for (message, arrival_ns) in computer_keys_rx {
                router.route(&message, forwarder.ticks_at(arrival_ns));
            }
        })
        .expect("Failed to spawn computerkeys thread")
}
