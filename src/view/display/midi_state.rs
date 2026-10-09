//! The settings modal's MIDI tab state — the input and output [`PortList`]s
//! (scanned ports, cursor, scroll window, connected port, reconnect channel
//! end), which row has the focus, and the editable output-offset mirror.
//!
//! Grouped out of [`Display`](super::Display) as `Display::midi`; every method that drives it
//! stays on `Display` and reaches in through `self.midi`. See
//! `020-views-and-state.md` and `160-midi-out-offset.md`.

use crossbeam_channel::Sender;

use crate::core::midi::port::matching_port_index;

use super::browser::scroll_to_show;
use super::settings_modal::window_start;
use super::{MidiSettingsFocus, PortSide};

/// Port rows a MIDI port list shows at once; a longer list scrolls.
pub(super) const PORT_ROWS: usize = 4;

/// One MIDI port list: input or output.
pub(super) struct PortList {
    /// The port names, refreshed live from `UiEvent::MidiPortsRefreshed`.
    pub(super) ports: Vec<String>,
    /// The cursor row.
    pub(super) selection: usize,
    /// First row of the `PORT_ROWS` window — moved only as far as it takes
    /// to keep the cursor in view, so a click never makes it jump.
    scroll: usize,
    /// The connected (or wanted) port, for the badge.
    pub(super) current: Option<String>,
    /// Fires an immediate reconnect on connect.
    reconnect_tx: Sender<String>,
}

impl PortList {
    /// An empty list connected to `current`.
    fn new(reconnect_tx: Sender<String>, current: Option<String>) -> Self {
        PortList {
            ports: Vec::new(),
            selection: 0,
            scroll: 0,
            current,
            reconnect_tx,
        }
    }

    /// Replaces the port names (hot-plug), keeping the cursor inside them.
    pub(super) fn set_ports(&mut self, ports: Vec<String>) {
        self.ports = ports;
        self.select(self.selection);
    }

    /// Puts the cursor on the connected port, or the first row.
    pub(super) fn select_current(&mut self) {
        self.select(matching_port_index(&self.ports, self.current.as_deref()).unwrap_or(0));
    }

    /// Puts the cursor on row `idx`, kept inside the list and in view.
    pub(super) fn select(&mut self, idx: usize) {
        self.selection = idx.min(self.ports.len().saturating_sub(1));
        self.scroll = scroll_to_show(self.scroll, self.selection, PORT_ROWS);
    }

    /// Moves the cursor by `step` rows, stopping at either end.
    pub(super) fn move_selection(&mut self, step: isize) {
        self.select(self.selection.saturating_add_signed(step));
    }

    /// The first row the window shows — pulled back when the list has
    /// shrunk under it.
    pub(super) fn window_start(&self) -> usize {
        window_start(self.scroll, self.ports.len(), PORT_ROWS)
    }

    /// How many port rows the window shows.
    pub(super) fn visible(&self) -> usize {
        self.ports.len().min(PORT_ROWS)
    }

    /// The index of the connected port in the list, if it is there.
    pub(super) fn current_index(&self) -> Option<usize> {
        matching_port_index(&self.ports, self.current.as_deref())
    }

    /// The connected port when it isn't in the list (unplugged) — shown as
    /// a ghost row under it.
    pub(super) fn missing(&self) -> Option<&str> {
        self.current
            .as_deref()
            .filter(|_| self.current_index().is_none())
    }

    /// The connected port's name, empty for none — what gets saved.
    pub(super) fn current_name(&self) -> String {
        self.current.clone().unwrap_or_default()
    }

    /// Connects the port under the cursor: it becomes the current one and
    /// the `"midiwatcher"` reconnects to it.
    pub(super) fn connect(&mut self) {
        let port = self.ports.get(self.selection).cloned().unwrap_or_default();
        self.current = Some(port.clone());
        self.reconnect_tx.send(port).ok();
    }
}

/// The settings modal's MIDI tab state, grouped out of [`Display`](super::Display).
pub(super) struct MidiSettingsState {
    /// The input-port list.
    pub(super) input: PortList,
    /// The output-port list.
    pub(super) output: PortList,
    /// Which row the arrow keys act on.
    pub(super) focus: MidiSettingsFocus,
    /// Editable mirror of the persisted MIDI-output offset, in milliseconds.
    /// Applied and saved on every nudge.
    pub(super) out_offset_ms: i32,
}

impl MidiSettingsState {
    /// Builds the tab state with its reconnect channel ends, the ports
    /// already connected at startup, and the persisted output offset. The port
    /// lists start empty and fill from the first `MidiPortsRefreshed`.
    pub(super) fn new(
        midi_in_reconnect_tx: Sender<String>,
        midi_out_reconnect_tx: Sender<String>,
        midi_in_current_port: Option<String>,
        midi_out_current_port: Option<String>,
        out_offset_ms: i32,
    ) -> Self {
        MidiSettingsState {
            input: PortList::new(midi_in_reconnect_tx, midi_in_current_port),
            output: PortList::new(midi_out_reconnect_tx, midi_out_current_port),
            focus: MidiSettingsFocus::InPort,
            out_offset_ms,
        }
    }

    /// The `side` port list.
    pub(super) fn list(&self, side: PortSide) -> &PortList {
        match side {
            PortSide::In => &self.input,
            PortSide::Out => &self.output,
        }
    }

    /// The `side` port list, to change.
    pub(super) fn list_mut(&mut self, side: PortSide) -> &mut PortList {
        match side {
            PortSide::In => &mut self.input,
            PortSide::Out => &mut self.output,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use super::{PORT_ROWS, PortList};

    /// A list of `names`, connected to `current`.
    fn list(names: &[&str], current: Option<&str>) -> PortList {
        let mut list = PortList::new(unbounded().0, current.map(str::to_owned));
        list.set_ports(names.iter().map(|name| name.to_string()).collect());
        list
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut ports = list(&["a", "b", "c"], None);
        ports.move_selection(-1);
        assert_eq!(ports.selection, 0);
        ports.move_selection(5);
        assert_eq!(ports.selection, 2);
    }

    #[test]
    fn the_window_follows_the_cursor_only_as_far_as_needed() {
        let names = ["a", "b", "c", "d", "e", "f", "g"];
        let mut ports = list(&names, None);
        ports.select(6);
        assert_eq!(ports.window_start(), 6 + 1 - PORT_ROWS);
        // A row already in view doesn't move the window.
        ports.select(4);
        assert_eq!(ports.window_start(), 6 + 1 - PORT_ROWS);
    }

    #[test]
    fn a_shrunk_list_keeps_the_cursor_and_window_inside_it() {
        let mut ports = list(&["a", "b", "c", "d", "e", "f"], None);
        ports.select(5);
        ports.set_ports(vec!["a".into(), "b".into()]);
        assert_eq!(ports.selection, 1);
        assert_eq!(ports.window_start(), 0);
    }

    #[test]
    fn an_unplugged_current_port_is_missing() {
        assert_eq!(list(&["a", "b"], Some("x")).missing(), Some("x"));
        assert_eq!(list(&["a", "b"], Some("b")).missing(), None);
        assert_eq!(list(&["a", "b"], None).missing(), None);
    }

    #[test]
    fn the_cursor_opens_on_the_current_port() {
        let mut ports = list(&["a", "b", "c"], Some("c"));
        ports.select_current();
        assert_eq!(ports.selection, 2);
    }
}
