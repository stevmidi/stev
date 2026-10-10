//! The modal overlays' enums: which overlay is up, and for the settings
//! modal which tab it shows and which MIDI row the arrow keys act on.
//!
//! They sit in their own module rather than beside a renderer because three
//! separate subtrees each own a piece: `input/modal.rs` moves the focus,
//! `settings_modal.rs` resets it on opening, and `rendering/modals/` draws
//! it.
//! See `020-views-and-state.md`.

/// The modal overlay that is up over the view — at most one at a time. It
/// takes every key and the pointer while it is; the view underneath is left
/// alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Overlay {
    /// The settings modal (`settings_modal.rs`).
    Settings,
    /// The keyboard help overlay (`help_overlay.rs`).
    Help,
    /// A project load restoring its plugins, one per frame
    /// (`instrument_restore.rs`). Takes no input of its own: it swallows
    /// every event until the last plugin is in, then closes itself.
    #[cfg(target_os = "macos")]
    RestoringInstruments,
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
    /// MIDI input / output ports and the output offset — the tab after
    /// launch.
    #[default]
    Midi,
    /// The colour theme list.
    Appearance,
}

impl SettingsTab {
    /// Every tab, in tab-strip order.
    pub(crate) const ALL: [SettingsTab; 2] = [SettingsTab::Midi, SettingsTab::Appearance];

    /// The tab strip's label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            SettingsTab::Midi => "MIDI",
            SettingsTab::Appearance => "APPEARANCE",
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
    use super::{MidiSettingsFocus, PortSide, SettingsTab};

    #[test]
    fn the_rows_cycle_back_to_the_input_list() {
        let mut focus = MidiSettingsFocus::InPort;
        for _ in 0..3 {
            focus = focus.next();
        }
        assert_eq!(focus, MidiSettingsFocus::InPort);
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
        assert_eq!(SettingsTab::Midi.stepped(1), SettingsTab::Appearance);
        assert_eq!(SettingsTab::Appearance.stepped(1), SettingsTab::Midi);
        assert_eq!(SettingsTab::Midi.stepped(-1), SettingsTab::Appearance);
        assert_eq!(SettingsTab::Appearance.stepped(-1), SettingsTab::Midi);
    }
}
