//! MIDI I/O: the live-input forwarder, the output connection, and the shared
//! port-name helpers they both rely on.

pub(crate) mod input;
mod input_tick;
pub(crate) mod message;
pub(crate) mod out_queue;
pub(crate) mod output;
pub(crate) mod port;
