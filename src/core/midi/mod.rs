//! MIDI I/O: the live-input forwarder (`input.rs`) and its sub-tick
//! timestamping (`input_tick.rs`), the output connection (`output.rs`) and its
//! deadline-ordered delay queue (`out_queue.rs`), the raw byte helpers
//! (`message.rs`), and the port-name helpers both sides rely on (`port.rs`).

pub(crate) mod input;
pub(crate) mod input_tick;
pub(crate) mod message;
pub(crate) mod out_queue;
pub(crate) mod output;
pub(crate) mod port;
