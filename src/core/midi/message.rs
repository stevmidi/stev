//! Raw MIDI byte helpers: the fixed-size [`Midi3`] the in-process instrument
//! route carries, note-edge and wheel parsing, which live input a take records,
//! channel rewriting and channel-message validation.

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

/// One of the two performance wheels a take records beside its notes
/// (`090-live-recording.md`): the pitch bend wheel and the mod wheel (CC1).
/// Every other controller stays live-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wheel {
    /// Pitch bend (`0xEn`), a 14-bit value centred on `0x2000`.
    PitchBend = 0,
    /// The mod wheel, Control Change 1 (`0xBn 01`), `0`–`127`, at rest at 0.
    Modulation = 1,
}

impl Wheel {
    /// Both wheels, in a fixed order — the index [`index`](Self::index) gives.
    pub(crate) const ALL: [Wheel; 2] = [Wheel::PitchBend, Wheel::Modulation];

    /// The wheel's position in [`ALL`](Self::ALL), for per-wheel arrays.
    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// Where the wheel rests: bend centred, mod wheel down.
    pub(crate) const fn neutral(self) -> u16 {
        match self {
            Wheel::PitchBend => 0x2000,
            Wheel::Modulation => 0,
        }
    }

    /// The channel-0 message that puts the wheel at `value` — the caller
    /// rewrites the channel for its output, as for any clip event.
    pub(crate) fn message(self, value: u16) -> Vec<u8> {
        match self {
            Wheel::PitchBend => vec![0xE0, (value & 0x7F) as u8, ((value >> 7) & 0x7F) as u8],
            Wheel::Modulation => vec![0xB0, 0x01, (value & 0x7F) as u8],
        }
    }
}

/// The wheel `msg` moves and the value it moves it to, or `None` for any
/// other message (another controller, a note, a short message).
pub(crate) fn parse_wheel(msg: &[u8]) -> Option<(Wheel, u16)> {
    let (&status, &data1, &data2) = (msg.first()?, msg.get(1)?, msg.get(2)?);
    match status & 0xF0 {
        0xE0 => Some((Wheel::PitchBend, pitch_bend_value([status, data1, data2]))),
        0xB0 if data1 == 0x01 => Some((Wheel::Modulation, u16::from(data2 & 0x7F))),
        _ => None,
    }
}

/// Reassembles pitch bend's 14-bit value from its LSB-then-MSB pair.
pub(crate) fn pitch_bend_value(bytes: Midi3) -> u16 {
    u16::from(bytes[1] & 0x7F) | (u16::from(bytes[2] & 0x7F) << 7)
}

/// Whether a take records live-input `msg`: a note edge or a wheel move
/// ([`parse_wheel`]). Everything else on the input is played live only.
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
        Wheel, is_channel_message, is_recorded, parse_note, parse_wheel, pitch_bend_value,
    };

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
            Some((Wheel::PitchBend, 0x2000))
        );
        assert_eq!(
            parse_wheel(&[0xE0, 0x7F, 0x7F]),
            Some((Wheel::PitchBend, 0x3FFF))
        );
        assert_eq!(
            parse_wheel(&[0xB5, 0x01, 90]),
            Some((Wheel::Modulation, 90))
        );
        // Other controllers (sustain, CC74), notes and short messages aren't.
        assert_eq!(parse_wheel(&[0xB0, 64, 127]), None);
        assert_eq!(parse_wheel(&[0xB0, 74, 64]), None);
        assert_eq!(parse_wheel(&[0x90, 60, 100]), None);
        assert_eq!(parse_wheel(&[0xE0, 0x00]), None);
    }

    #[test]
    fn pitch_bend_value_reassembles_lsb_then_msb() {
        assert_eq!(pitch_bend_value([0xE0, 0x00, 0x40]), 8192);
        assert_eq!(pitch_bend_value([0xE0, 0x01, 0x00]), 1);
        assert_eq!(pitch_bend_value([0xE0, 0x7F, 0x7F]), 16383);
    }

    #[test]
    fn a_wheel_message_parses_back_to_its_value() {
        for (wheel, value) in [
            (Wheel::PitchBend, 0),
            (Wheel::PitchBend, 0x2000),
            (Wheel::PitchBend, 0x3FFF),
            (Wheel::Modulation, 0),
            (Wheel::Modulation, 127),
        ] {
            assert_eq!(parse_wheel(&wheel.message(value)), Some((wheel, value)));
        }
        for wheel in Wheel::ALL {
            assert_eq!(Wheel::ALL[wheel.index()], wheel);
        }
    }

    #[test]
    fn a_take_records_note_edges_and_the_two_wheels() {
        assert!(is_recorded(&[0x90, 60, 100]));
        assert!(is_recorded(&[0x80, 60, 0]));
        assert!(is_recorded(&[0xE0, 0x00, 0x50]));
        assert!(is_recorded(&[0xB0, 0x01, 64]));
        assert!(!is_recorded(&[0xB0, 64, 127]));
        assert!(!is_recorded(&[0xC0, 5]));
        assert!(!is_recorded(&[0xD0, 40]));
        assert!(!is_recorded(&[0xF8]));
    }
}
