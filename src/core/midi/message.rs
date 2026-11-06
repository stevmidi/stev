//! Raw MIDI byte helpers: the fixed-size [`Midi3`] the in-process instrument
//! route carries, note-edge and wheel parsing, which live input a take records,
//! channel rewriting and channel-message validation.

use crate::models::knobs::PluginParam;

/// A MIDI channel-voice message, fixed at 3 bytes: shorter messages (Program
/// Change, Channel Pressure) are zero-padded, longer ones (SysEx — filtered out
/// upstream anyway) keep only their first 3 bytes. Inline, so an event crossing
/// an `rtrb` ring to the audio thread carries no heap allocation to free there.
pub(crate) type Midi3 = [u8; 3];

/// Truncates / zero-pads a raw MIDI message to [`Midi3`].
pub(crate) fn midi3(bytes: &[u8]) -> Midi3 {
    [
        bytes.first().copied().unwrap_or(0),
        bytes.get(1).copied().unwrap_or(0),
        bytes.get(2).copied().unwrap_or(0),
    ]
}

/// A Control Change's status byte, on channel 0.
pub(crate) const CONTROL_CHANGE: u8 = 0xB0;

/// The mod wheel's controller number — never a knob's (`models::knobs`).
pub(crate) const MOD_WHEEL_CC: u8 = 0x01;

/// The channel-0 Control Change that puts controller `cc` at `value` — the
/// caller rewrites the channel for its output.
pub(crate) fn control_change(cc: u8, value: u8) -> Midi3 {
    [CONTROL_CHANGE, cc & 0x7F, value & 0x7F]
}

/// A parsed note edge: `Some((note, is_on))` for note-on / note-off (a
/// velocity-0 note-on counts as off), `None` for anything else.
pub(crate) fn parse_note(msg: &[u8]) -> Option<(usize, bool)> {
    let (&status, &note) = (msg.first()?, msg.get(1)?);
    let velocity = msg.get(2).copied().unwrap_or(0);
    let note = (note & 0x7F) as usize;
    match status & 0xF0 {
        0x90 if velocity > 0 => Some((note, true)),
        0x80 | 0x90 => Some((note, false)),
        _ => None,
    }
}

/// A performance controller a take records beside its notes
/// (`090-recording-midi-input.md`): pitch bend, a Control Change by number —
/// the mod wheel (CC1) among them, and any other CC a clip holds (pasted,
/// imported, an older project's CC knobs) — or a plugin parameter a knob
/// drives. The two wheels have a rest a stop returns them to; anything else
/// has none and stays where it was. Ordered pitch bend, CCs by number,
/// parameters by ID: the order a track chases and a clip lists them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Controller {
    /// Pitch bend (`0xEn`), a 14-bit value centred on `0x2000`.
    PitchBend,
    /// Control Change `n` (`0xBn n`), `0`–`127` — never a channel mode
    /// message (120–127), which [`parse_controller`] doesn't read as one.
    Cc(u8),
    /// A parameter of the track's plugin, by ID, normalized to `0.0..=1.0`
    /// (`400-parameter-knobs.md`). Not MIDI: no message carries it, so
    /// [`parse_controller`] never returns one. A clip holds its moves as
    /// [`ParamMove`](crate::models::event::ParamMove) events.
    Param(PluginParam),
}

impl Controller {
    /// The mod wheel, CC1, at rest at 0.
    pub(crate) const MOD_WHEEL: Controller = Controller::Cc(MOD_WHEEL_CC);

    /// The two wheels, in the order the wheel lanes list them
    /// ([`wheel_index`](Self::wheel_index)).
    pub(crate) const WHEELS: [Controller; 2] = [Controller::PitchBend, Controller::MOD_WHEEL];

    /// Whether the controller is one of the two wheels — the ones with a
    /// [`rest`](Self::rest).
    pub(crate) const fn is_wheel(self) -> bool {
        self.rest().is_some()
    }

    /// The controller's position in [`WHEELS`](Self::WHEELS), `None` for a
    /// CC that isn't the mod wheel.
    pub(crate) fn wheel_index(self) -> Option<usize> {
        Self::WHEELS.iter().position(|&wheel| wheel == self)
    }

    /// Where the controller rests, if it has a rest: bend centred, mod wheel
    /// down. `None` for any other CC or a parameter — a filter cutoff "at
    /// rest" at 0 would be wrong, so it is never reset (`archive/390-knob-panel.md`
    /// decision 8, `400-parameter-knobs.md` decision 9).
    pub(crate) const fn rest(self) -> Option<u16> {
        match self {
            Controller::PitchBend => Some(PITCH_BEND_CENTRE),
            Controller::Cc(MOD_WHEEL_CC) => Some(0),
            Controller::Cc(_) | Controller::Param(_) => None,
        }
    }

    /// [`rest`](Self::rest) as the `f64` a value in force is.
    pub(crate) fn rest_value(self) -> Option<f64> {
        self.rest().map(f64::from)
    }

    /// The channel-0 message that puts the controller at `value` — the
    /// caller rewrites the channel for its output, as for any clip event.
    /// Empty for a parameter, which no MIDI message moves:
    /// [`Event::controller_move_at`](crate::models::event::Event::controller_move_at)
    /// builds a move of any controller.
    pub(crate) fn message(self, value: u16) -> Vec<u8> {
        match self {
            Controller::PitchBend => {
                vec![0xE0, (value & 0x7F) as u8, ((value >> 7) & 0x7F) as u8]
            }
            Controller::Cc(cc) => control_change(cc, value as u8).to_vec(),
            Controller::Param(_) => Vec::new(),
        }
    }
}

/// Pitch bend's rest: centred.
pub(crate) const PITCH_BEND_CENTRE: u16 = 0x2000;

/// The first of the channel mode messages (120–127): Control Change numbers
/// that switch a synth's modes rather than move a controller.
pub(crate) const FIRST_CHANNEL_MODE_CC: u8 = 120;

/// The controller `msg` moves and the value it moves it to, or `None` for
/// anything else (a note, a channel mode message, a short message).
pub(crate) fn parse_controller(msg: &[u8]) -> Option<(Controller, u16)> {
    let (&status, &data1, &data2) = (msg.first()?, msg.get(1)?, msg.get(2)?);
    match status & 0xF0 {
        0xE0 => Some((
            Controller::PitchBend,
            pitch_bend_value([status, data1, data2]),
        )),
        CONTROL_CHANGE if data1 & 0x7F < FIRST_CHANNEL_MODE_CC => {
            Some((Controller::Cc(data1 & 0x7F), u16::from(data2 & 0x7F)))
        }
        _ => None,
    }
}

/// The wheel `msg` moves — pitch bend or the mod wheel — and its value, or
/// `None` for anything else, any other CC included.
pub(crate) fn parse_wheel(msg: &[u8]) -> Option<(Controller, u16)> {
    parse_controller(msg).filter(|(controller, _)| controller.is_wheel())
}

/// A 7-bit MIDI data byte as `0.0..=1.0`, the form plugin formats take.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn unit_7bit(byte: u8) -> f64 {
    f64::from(byte & 0x7F) / 127.0
}

/// Reassembles pitch bend's 14-bit value from its LSB-then-MSB pair.
pub(crate) fn pitch_bend_value(bytes: Midi3) -> u16 {
    u16::from(bytes[1] & 0x7F) | (u16::from(bytes[2] & 0x7F) << 7)
}

/// Whether a take records live-input `msg`: a note edge or a wheel move
/// ([`parse_wheel`]). Everything else on the input is played live only — a
/// hardware controller's CC among it.
pub(crate) fn is_recorded(msg: &[u8]) -> bool {
    parse_note(msg).is_some() || parse_wheel(msg).is_some()
}

/// Whether `message` is a complete channel voice message: status `0x80`–`0xEF`
/// followed by its one (program change, channel pressure) or two data bytes.
pub(crate) fn is_channel_message(message: &[u8]) -> bool {
    let Some(&status) = message.first() else {
        return false;
    };
    let data_len = match status & 0xF0 {
        0xC0 | 0xD0 => 1,
        0x80..=0xE0 => 2,
        _ => return false,
    };
    message.len() == 1 + data_len && message[1..].iter().all(|byte| byte & 0x80 == 0)
}

/// Rewrites the channel nibble of a channel-voice message in place; leaves
/// System Common / Realtime messages (`>= 0xF0`) untouched.
pub(crate) fn rewrite_channel(msg: &mut [u8], midi_channel: u8) {
    if let Some(status) = msg.get_mut(0) {
        // Do not rewrite System Common / Realtime
        if *status >= 0xF0 {
            return;
        }
        *status = (*status & 0xF0) | (midi_channel & 0x0F);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Controller, is_channel_message, is_recorded, parse_controller, parse_note, parse_wheel,
        pitch_bend_value, unit_7bit,
    };
    use crate::models::knobs::test_param;

    #[test]
    fn unit_7bit_spans_zero_to_one_and_ignores_the_top_bit() {
        assert_eq!(unit_7bit(0), 0.0);
        assert_eq!(unit_7bit(127), 1.0);
        assert_eq!(unit_7bit(0x80), 0.0);
    }

    #[test]
    fn parse_note_reads_edges_and_treats_velocity_zero_as_off() {
        assert_eq!(parse_note(&[0x93, 60, 100]), Some((60, true)));
        assert_eq!(parse_note(&[0x93, 60, 0]), Some((60, false)));
        assert_eq!(parse_note(&[0x83, 60, 64]), Some((60, false)));
        assert_eq!(parse_note(&[0xB0, 74, 64]), None);
        assert_eq!(parse_note(&[0x90]), None);
    }

    #[test]
    fn is_channel_message_wants_a_channel_status_and_its_data_bytes() {
        assert!(is_channel_message(&[0x93, 60, 100]));
        assert!(is_channel_message(&[0xC0, 5]));
        assert!(is_channel_message(&[0xEF, 0, 64]));
        assert!(!is_channel_message(&[0x90, 60]));
        assert!(!is_channel_message(&[0xC0, 5, 0]));
        assert!(!is_channel_message(&[0x90, 60, 0x80]));
        assert!(!is_channel_message(&[0xF8]));
        assert!(!is_channel_message(&[]));
    }

    #[test]
    fn parse_wheel_reads_bend_and_the_mod_wheel_only() {
        assert_eq!(
            parse_wheel(&[0xE3, 0x00, 0x40]),
            Some((Controller::PitchBend, 0x2000))
        );
        assert_eq!(
            parse_wheel(&[0xE0, 0x7F, 0x7F]),
            Some((Controller::PitchBend, 0x3FFF))
        );
        assert_eq!(
            parse_wheel(&[0xB5, 0x01, 90]),
            Some((Controller::MOD_WHEEL, 90))
        );
        // Other controllers (sustain, CC74), notes and short messages aren't.
        assert_eq!(parse_wheel(&[0xB0, 64, 127]), None);
        assert_eq!(parse_wheel(&[0xB0, 74, 64]), None);
        assert_eq!(parse_wheel(&[0x90, 60, 100]), None);
        assert_eq!(parse_wheel(&[0xE0, 0x00]), None);
    }

    #[test]
    fn parse_controller_reads_any_cc_but_the_channel_mode_messages() {
        assert_eq!(
            parse_controller(&[0xB2, 14, 90]),
            Some((Controller::Cc(14), 90))
        );
        assert_eq!(
            parse_controller(&[0xB0, 0x01, 5]),
            Some((Controller::MOD_WHEEL, 5))
        );
        assert_eq!(
            parse_controller(&[0xE0, 0x00, 0x40]),
            Some((Controller::PitchBend, 0x2000))
        );
        assert_eq!(
            parse_controller(&[0xB0, 119, 1]),
            Some((Controller::Cc(119), 1))
        );
        assert_eq!(parse_controller(&[0xB0, 120, 0]), None);
        assert_eq!(parse_controller(&[0xB0, 123, 0]), None);
        assert_eq!(parse_controller(&[0x90, 60, 100]), None);
    }

    #[test]
    fn only_the_two_wheels_rest() {
        assert_eq!(Controller::PitchBend.rest(), Some(0x2000));
        assert_eq!(Controller::MOD_WHEEL.rest(), Some(0));
        assert_eq!(Controller::Cc(14).rest(), None);
        assert_eq!(Controller::Cc(0).rest(), None);
    }

    #[test]
    fn pitch_bend_value_reassembles_lsb_then_msb() {
        assert_eq!(pitch_bend_value([0xE0, 0x00, 0x40]), 8192);
        assert_eq!(pitch_bend_value([0xE0, 0x01, 0x00]), 1);
        assert_eq!(pitch_bend_value([0xE0, 0x7F, 0x7F]), 16383);
    }

    #[test]
    fn a_controller_message_parses_back_to_its_value() {
        for (controller, value) in [
            (Controller::PitchBend, 0),
            (Controller::PitchBend, 0x2000),
            (Controller::PitchBend, 0x3FFF),
            (Controller::MOD_WHEEL, 0),
            (Controller::MOD_WHEEL, 127),
            (Controller::Cc(14), 64),
            (Controller::Cc(0), 127),
        ] {
            assert_eq!(
                parse_controller(&controller.message(value)),
                Some((controller, value))
            );
        }
        assert!(Controller::PitchBend < Controller::Cc(0));
        assert!(Controller::Cc(127) < Controller::Param(test_param(0)));
        for (i, wheel) in Controller::WHEELS.into_iter().enumerate() {
            assert_eq!(wheel.wheel_index(), Some(i));
        }
        assert_eq!(Controller::Cc(14).wheel_index(), None);
    }

    #[test]
    fn a_take_records_note_edges_and_the_two_wheels() {
        assert!(is_recorded(&[0x90, 60, 100]));
        assert!(is_recorded(&[0x80, 60, 0]));
        assert!(is_recorded(&[0xE0, 0x00, 0x50]));
        assert!(is_recorded(&[0xB0, 0x01, 64]));
        assert!(!is_recorded(&[0xB0, 123, 0]));
        assert!(!is_recorded(&[0xC0, 5]));
        assert!(!is_recorded(&[0xD0, 40]));
        assert!(!is_recorded(&[0xF8]));
        // A hardware CC plays live only.
        assert!(!is_recorded(&[0xB0, 14, 127]));
    }
}
