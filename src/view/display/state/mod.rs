//! `Display`'s render-side state upkeep: `ui_events.rs` drains `ui_event_rx` and
//! reconciles the projection, `shapes.rs` keeps the clip / event shape lists in
//! step, `scroll.rs` owns the arranger horizontal scroll and its cursor-follow
//! paging, `zoom.rs` its horizontal zoom. See `020-views-and-state.md`,
//! `030-ui-design.md`, `archive/190-arranger-zoom.md`.

mod scroll;
mod shapes;
mod ui_events;
mod zoom;

pub(super) use scroll::scrolled_offset;

use super::*;
