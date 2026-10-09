//! `handle_help_input_event` — the help overlay's keymap and mouse: `?` or
//! Esc closes it, so does a click anywhere, and the wheel scrolls a page
//! taller than the window. See `help_overlay.rs` and
//! `020-views-and-state.md`.

use crate::view::display::help_overlay::is_help_key;

use super::*;

impl Display {
    /// An event while the help overlay is up — it takes every event, but
    /// only `?`, Esc, a press and the wheel do anything.
    pub(super) fn handle_help_input_event(&mut self, input_event: &InputEvent) {
        match *input_event {
            InputEvent::KeyPressed { key, modifiers }
                if key == Key::Escape || is_help_key(key, modifiers) =>
            {
                self.close_overlay();
            }
            InputEvent::MouseClicked { .. } => self.close_overlay(),
            InputEvent::TimelineScroll { delta_y, .. } => self.scroll_help(delta_y),
            _ => {}
        }
    }
}
