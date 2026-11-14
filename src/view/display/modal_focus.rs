//! The modal overlays' enums: which overlay is up, and for the settings
//! modal which tab it shows and which Audio or MIDI section the keys act on.
//!
//! They sit in their own module rather than beside a renderer because three
//! separate subtrees each own a piece: `input/modal.rs` moves the focus,
//! `settings/mod.rs` resets it on opening, and `rendering/modals/` draws
//! it.
//! See `020-views-and-state.md`.

/// The modal overlay that is up over the view — at most one at a time. It
/// takes every key and the pointer while it is; the view underneath is left
/// alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Overlay {
    /// The settings modal (`settings/mod.rs`).
    Settings,
    /// A project load restoring its plugins, one per frame
    /// (`instrument_restore.rs`). Takes no input of its own: it swallows
    /// every event until the last plugin is in, then closes itself. Restore
    /// puts it up ahead of the staging, to hold that gap too.
    #[cfg(target_os = "macos")]
    RestoringInstruments,
}

/// The settings modal's Audio tab: which section the keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AudioSettingsFocus {
    /// The output-device list: ↑/↓ move its cursor.
    #[default]
    Device,
    /// The sample rates: ←/→ move along them.
    Rate,
    /// The buffer sizes: ←/→ move along them.
    Buffer,
}

impl AudioSettingsFocus {
    /// The next section, as Tab and ↓ from a row of choices move: OUTPUT
    /// DEVICE → SAMPLE RATE → BUFFER SIZE → OUTPUT DEVICE.
    pub(crate) fn next(self) -> AudioSettingsFocus {
        match self {
            AudioSettingsFocus::Device => AudioSettingsFocus::Rate,
            AudioSettingsFocus::Rate => AudioSettingsFocus::Buffer,
            AudioSettingsFocus::Buffer => AudioSettingsFocus::Device,
        }
    }

    /// The previous section, as ⇧Tab and ↑ from a row of choices move: the
    /// reverse of [`next`](Self::next).
    pub(crate) fn previous(self) -> AudioSettingsFocus {
        match self {
            AudioSettingsFocus::Device => AudioSettingsFocus::Buffer,
            AudioSettingsFocus::Rate => AudioSettingsFocus::Device,
            AudioSettingsFocus::Buffer => AudioSettingsFocus::Rate,
        }
    }
}

/// The settings modal's MIDI tab: which row the arrow keys act on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MidiSettingsFocus {
    /// The input-port list.
    InPort,
    /// The output-port list.
    OutPort,
    /// The MIDI-output offset row — a number, not a list, so the arrow keys
    /// nudge the value instead of moving a cursor. See `160-midi-out-offset.md`.
    OutOffset,
}

impl MidiSettingsFocus {
    /// The next row, as Tab / ←/→ cycle them: IN PORT → OUT PORT → OUT
    /// OFFSET → IN PORT.
    pub(crate) fn next(self) -> MidiSettingsFocus {
        match self {
            MidiSettingsFocus::InPort => MidiSettingsFocus::OutPort,
            MidiSettingsFocus::OutPort => MidiSettingsFocus::OutOffset,
            MidiSettingsFocus::OutOffset => MidiSettingsFocus::InPort,
        }
    }

    /// The previous row, as ⇧Tab cycles them: the reverse of
    /// [`next`](Self::next).
    pub(crate) fn previous(self) -> MidiSettingsFocus {
        match self {
            MidiSettingsFocus::InPort => MidiSettingsFocus::OutOffset,
            MidiSettingsFocus::OutPort => MidiSettingsFocus::InPort,
            MidiSettingsFocus::OutOffset => MidiSettingsFocus::OutPort,
        }
    }

    /// The port list this row is — `None` for the offset row.
    pub(crate) fn port_side(self) -> Option<PortSide> {
        match self {
            MidiSettingsFocus::InPort => Some(PortSide::In),
            MidiSettingsFocus::OutPort => Some(PortSide::Out),
            MidiSettingsFocus::OutOffset => None,
        }
    }
}

/// One of the MIDI tab's two port lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortSide {
    /// The input-port list.
    In,
    /// The output-port list.
    Out,
}

impl PortSide {
    /// Both lists, top to bottom.
    pub(crate) const ALL: [PortSide; 2] = [PortSide::In, PortSide::Out];

    /// The focus row this list is.
    pub(crate) fn focus(self) -> MidiSettingsFocus {
        match self {
            PortSide::In => MidiSettingsFocus::InPort,
            PortSide::Out => MidiSettingsFocus::OutPort,
        }
    }

    /// The list's section header.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PortSide::In => "IN PORT",
            PortSide::Out => "OUT PORT",
        }
    }
}

/// The settings modal's tabs, in tab-strip order. Adding one is a variant
/// here, its label, and a draw function (`rendering/modals/settings.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsTab {
    /// The output device, sample rate and buffer size, and what runs
    /// (`settings/audio_tab.rs`, `480-audio-settings.md`) — the tab after
    /// launch.
    #[default]
    Audio,
    /// MIDI input / output ports and the output offset.
    Midi,
    /// The colour theme list.
    Appearance,
    /// Every command and its keys, with the fixed keys after them
    /// (`settings/keyboard_tab.rs`).
    Keyboard,
    /// *Allow AI agents*, what it does and how to connect a client
    /// (`settings/ai_tab.rs`, `360-mcp-server.md`).
    Ai,
    /// The version, the links, the VST logo and (macOS) Check for updates
    /// (`settings/about_tab.rs`).
    About,
}

impl SettingsTab {
    /// Every tab, in tab-strip order.
    pub(crate) const ALL: [SettingsTab; 6] = [
        SettingsTab::Audio,
        SettingsTab::Midi,
        SettingsTab::Appearance,
        SettingsTab::Keyboard,
        SettingsTab::Ai,
        SettingsTab::About,
    ];

    /// The tab strip's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsTab::Audio => "AUDIO",
            SettingsTab::Midi => "MIDI",
            SettingsTab::Appearance => "APPEARANCE",
            SettingsTab::Keyboard => "KEYBOARD",
            SettingsTab::Ai => "AI",
            SettingsTab::About => "ABOUT",
        }
    }

    /// The tab `step` places along the strip, wrapping at either end.
    pub(crate) fn stepped(self, step: isize) -> SettingsTab {
        let len = Self::ALL.len() as isize;
        let idx = Self::ALL.iter().position(|&tab| tab == self).unwrap_or(0) as isize;
        Self::ALL[(idx + step).rem_euclid(len) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioSettingsFocus, MidiSettingsFocus, PortSide, SettingsTab};

    #[test]
    fn the_audio_sections_cycle_both_ways() {
        let mut focus = AudioSettingsFocus::Device;
        for _ in 0..3 {
            assert_eq!(focus.next().previous(), focus);
            focus = focus.next();
        }
        assert_eq!(focus, AudioSettingsFocus::Device);
    }

    #[test]
    fn the_rows_cycle_back_to_the_input_list() {
        let mut focus = MidiSettingsFocus::InPort;
        for _ in 0..3 {
            focus = focus.next();
        }
        assert_eq!(focus, MidiSettingsFocus::InPort);
    }

    #[test]
    fn previous_undoes_next() {
        for focus in [
            MidiSettingsFocus::InPort,
            MidiSettingsFocus::OutPort,
            MidiSettingsFocus::OutOffset,
        ] {
            assert_eq!(focus.next().previous(), focus);
        }
        assert_eq!(
            MidiSettingsFocus::InPort.previous(),
            MidiSettingsFocus::OutOffset
        );
    }

    #[test]
    fn a_port_row_and_its_list_name_each_other() {
        for side in PortSide::ALL {
            assert_eq!(side.focus().port_side(), Some(side));
        }
        assert_eq!(MidiSettingsFocus::OutOffset.port_side(), None);
    }

    #[test]
    fn stepping_a_tab_wraps_at_both_ends() {
        assert_eq!(SettingsTab::Audio.stepped(1), SettingsTab::Midi);
        assert_eq!(SettingsTab::Midi.stepped(1), SettingsTab::Appearance);
        assert_eq!(SettingsTab::Appearance.stepped(1), SettingsTab::Keyboard);
        assert_eq!(SettingsTab::Keyboard.stepped(1), SettingsTab::Ai);
        assert_eq!(SettingsTab::Ai.stepped(1), SettingsTab::About);
        assert_eq!(SettingsTab::About.stepped(1), SettingsTab::Audio);
        assert_eq!(SettingsTab::Audio.stepped(-1), SettingsTab::About);
        assert_eq!(SettingsTab::Midi.stepped(-1), SettingsTab::Audio);
        assert_eq!(SettingsTab::Keyboard.stepped(-1), SettingsTab::Appearance);
    }
}
