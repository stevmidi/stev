//! Raw MIDI byte helpers: the fixed-size [`Midi3`] the in-process instrument
//! route carries, note-edge and controller parsing, which live input a take records,
//! channel rewriting and channel-message validation.

use crate::core::config::{LANE_OCTAVE_DOWN_CC, LANE_OCTAVE_UP_CC};
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

/// The sustain pedal's controller number (`archive/530-sustain-capture.md`).
pub(crate) const SUSTAIN_CC: u8 = 64;

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
/// drives. The two wheels and the sustain pedal (CC64) have a rest a stop
/// returns them to ([`RESTING`](Self::RESTING)); anything else has none and
/// stays where it was. Ordered pitch bend, CCs by number,
/// parameters by ID: the order a track chases and a clip lists them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Controller {
    /// Pitch bend (`0xEn`), a 14-bit value centred on `0x2000`.
    PitchBend,
    /// Control Change `n` (`0xBn n`), `0`–`127` — never a channel mode
    /// message (120–127), which [`parse_controller`] doesn't read as one.
    Cc(u8),
    /// A parameter of the track's plugin, by ID, normalized to `0.0..=1.0`
    /// (`archive/400-parameter-knobs.md`). Not MIDI: no message carries it, so
    /// [`parse_controller`] never returns one. A clip holds its moves as
    /// [`ParamMove`](crate::models::event::ParamMove) events.
    Param(PluginParam),
}

impl Controller {
    /// The mod wheel, CC1, at rest at 0.
    pub(crate) const MOD_WHEEL: Controller = Controller::Cc(MOD_WHEEL_CC);

    /// The sustain pedal, CC64, at rest up (0).
    pub(crate) const SUSTAIN: Controller = Controller::Cc(SUSTAIN_CC);

    /// The controllers with a [`rest`](Self::rest) — the two wheels and the
    /// sustain pedal — in the order the lanes list them
    /// ([`resting_index`](Self::resting_index)): the ones a take records, a
    /// track chases back to rest and a stop resets.
    pub(crate) const RESTING: [Controller; 3] = [
        Controller::PitchBend,
        Controller::MOD_WHEEL,
        Controller::SUSTAIN,
    ];

    /// Whether the controller is one of [`RESTING`](Self::RESTING) — the
    /// ones with a [`rest`](Self::rest).
    pub(crate) const fn is_resting(self) -> bool {
        self.rest().is_some()
    }

    /// The controller's position in [`RESTING`](Self::RESTING), `None` for
    /// one without a rest.
    pub(crate) fn resting_index(self) -> Option<usize> {
        Self::RESTING.iter().position(|&resting| resting == self)
    }

    /// Where the controller rests, if it has a rest: bend centred, mod wheel
    /// down, the pedal up. Values are kept as sent, 0–127 for the pedal, with
    /// no threshold at 64: a continuous pedal's half-pedalling plays as
    /// played. `None` for any other CC or a parameter — a filter cutoff "at
    /// rest" at 0 would be wrong, so it is never reset (`archive/390-knob-panel.md`
    /// decision 8, `archive/400-parameter-knobs.md` decision 9).
    pub(crate) const fn rest(self) -> Option<u16> {
        match self {
            Controller::PitchBend => Some(PITCH_BEND_CENTRE),
            Controller::Cc(MOD_WHEEL_CC | SUSTAIN_CC) => Some(0),
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

/// The controller with a rest `msg` moves — pitch bend, the mod wheel or the
/// sustain pedal ([`Controller::RESTING`]) — and its value, or `None` for
/// anything else, any other CC included.
pub(crate) fn parse_resting(msg: &[u8]) -> Option<(Controller, u16)> {
    parse_controller(msg).filter(|(controller, _)| controller.is_resting())
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

/// Whether a take records live-input `msg`: a note edge, a wheel move or
/// the sustain pedal ([`parse_resting`]). Everything else on the input is
/// played live only — a hardware controller's CC among it.
pub(crate) fn is_recorded(msg: &[u8]) -> bool {
    parse_note(msg).is_some() || parse_resting(msg).is_some()
}

/// Whether `msg` is one of the performance lane's octave buttons
/// ([`LANE_OCTAVE_DOWN_CC`], [`LANE_OCTAVE_UP_CC`]) on any channel, press
/// or release: what goes to the lane's handler while it is armed
/// (`570-lane-octave-buttons.md` § 3).
pub(crate) fn is_lane_octave_button(msg: &[u8]) -> bool {
    matches!(
        parse_controller(msg),
        Some((Controller::Cc(LANE_OCTAVE_DOWN_CC | LANE_OCTAVE_UP_CC), _))
    )
}

/// A press of the performance lane's octave buttons: `Some(-1)` for an
/// octave down, `Some(1)` up, on any value above 0 and any channel; `None`
/// for the release (0) and anything else (`570-lane-octave-buttons.md` § 3).
pub(crate) fn lane_octave(msg: &[u8]) -> Option<i32> {
    match parse_controller(msg)? {
        (Controller::Cc(LANE_OCTAVE_DOWN_CC), 1..) => Some(-1),
        (Controller::Cc(LANE_OCTAVE_UP_CC), 1..) => Some(1),
        _ => None,
    }
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
        Controller, is_channel_message, is_lane_octave_button, is_recorded, lane_octave,
        parse_controller, parse_note, parse_resting, pitch_bend_value, unit_7bit,
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
    fn parse_resting_reads_bend_the_mod_wheel_and_the_pedal_only() {
        assert_eq!(
            parse_resting(&[0xE3, 0x00, 0x40]),
            Some((Controller::PitchBend, 0x2000))
        );
        assert_eq!(
            parse_resting(&[0xE0, 0x7F, 0x7F]),
            Some((Controller::PitchBend, 0x3FFF))
        );
        assert_eq!(
            parse_resting(&[0xB5, 0x01, 90]),
            Some((Controller::MOD_WHEEL, 90))
        );
        assert_eq!(
            parse_resting(&[0xB2, 64, 127]),
            Some((Controller::SUSTAIN, 127))
        );
        // Other controllers (sostenuto, CC74), notes and short messages aren't.
        assert_eq!(parse_resting(&[0xB0, 66, 127]), None);
        assert_eq!(parse_resting(&[0xB0, 74, 64]), None);
        assert_eq!(parse_resting(&[0x90, 60, 100]), None);
        assert_eq!(parse_resting(&[0xE0, 0x00]), None);
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
    fn only_the_two_wheels_and_the_pedal_rest() {
        assert_eq!(Controller::PitchBend.rest(), Some(0x2000));
        assert_eq!(Controller::MOD_WHEEL.rest(), Some(0));
        assert_eq!(Controller::SUSTAIN.rest(), Some(0));
        assert_eq!(Controller::Cc(66).rest(), None);
        assert_eq!(Controller::Cc(14).rest(), None);
        assert_eq!(Controller::Cc(0).rest(), None);
    }

    /// `RESTING` lists exactly the controllers `rest` gives a rest, so none
    /// is recorded but never reset, or reset but never chased.
    #[test]
    fn resting_lists_exactly_the_controllers_with_a_rest() {
        let midi = std::iter::once(Controller::PitchBend).chain((0..128).map(Controller::Cc));
        for controller in midi {
            assert_eq!(
                controller.is_resting(),
                Controller::RESTING.contains(&controller),
                "{controller:?}"
            );
        }
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
        for (i, resting) in Controller::RESTING.into_iter().enumerate() {
            assert_eq!(resting.resting_index(), Some(i));
        }
        assert_eq!(Controller::Cc(14).resting_index(), None);
    }

    #[test]
    fn a_take_records_note_edges_the_two_wheels_and_the_pedal() {
        assert!(is_recorded(&[0x90, 60, 100]));
        assert!(is_recorded(&[0x80, 60, 0]));
        assert!(is_recorded(&[0xE0, 0x00, 0x50]));
        assert!(is_recorded(&[0xB0, 0x01, 64]));
        // Half-pedalling and a lift alike.
        assert!(is_recorded(&[0xB0, 64, 40]));
        assert!(is_recorded(&[0xB0, 64, 0]));
        assert!(!is_recorded(&[0xB0, 66, 127]));
        assert!(!is_recorded(&[0xB0, 123, 0]));
        assert!(!is_recorded(&[0xC0, 5]));
        assert!(!is_recorded(&[0xD0, 40]));
        assert!(!is_recorded(&[0xF8]));
        // A hardware CC plays live only.
        assert!(!is_recorded(&[0xB0, 14, 127]));
    }

    #[test]
    fn the_lane_octave_buttons_step_on_a_press_only() {
        assert_eq!(lane_octave(&[0xB0, 28, 127]), Some(-1), "previous: down");
        assert_eq!(lane_octave(&[0xB0, 29, 127]), Some(1), "next: up");
        assert_eq!(lane_octave(&[0xB0, 29, 1]), Some(1), "any value above 0");
        assert_eq!(lane_octave(&[0xB0, 28, 0]), None, "the release");
        assert!(is_lane_octave_button(&[0xB0, 28, 0]), "still the button");
    }

    #[test]
    fn the_lane_octave_buttons_on_any_channel_and_no_other_cc() {
        assert_eq!(lane_octave(&[0xBF, 28, 127]), Some(-1));
        assert_eq!(lane_octave(&[0xB5, 29, 127]), Some(1));
        for msg in [
            [0xB0, 27, 127],
            [0xB0, 30, 127],
            [0x90, 28, 127],
            [0xE0, 29, 127],
        ] {
            assert_eq!(lane_octave(&msg), None, "{msg:?}");
            assert!(!is_lane_octave_button(&msg), "{msg:?}");
        }
        assert!(!is_lane_octave_button(&[0xB0, 28]), "short");
    }
}
