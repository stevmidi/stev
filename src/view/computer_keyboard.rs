//! The computer keyboard: the letter rows played as a piano, the common DAW
//! "computer MIDI keyboard" (Ableton's layout), for a machine with no MIDI
//! keyboard attached. Toggled by `⌘/Ctrl+K` or the header's keyboard chip.
//!
//! While it is on, the note keys below are taken by physical position (the
//! key's place on a US layout, whatever the active layout prints on it), so
//! the piano sits in the same place on every keyboard. Each press and release
//! becomes a note-on / note-off that `Display` sends to the `"computerkeys"`
//! thread, which routes it exactly as a MIDI port's input — thru, the plugin
//! tap, capture, recording. This module is the pure part: which keys are
//! taken ([`takes_key_event`]) and what each one plays ([`ComputerKeyboard`]).
//! See `010-keybindings.md` § Computer keyboard.

use egui::{Key, Modifiers};

use crate::core::input_event::KeyModifiers;
use crate::core::keymap::Chord;
use crate::core::midi::message::Midi3;

/// The note keys, lowest first: the home row is the white keys from C, the
/// row above it the black keys between them — `A` C, `W` C♯, `S` D … `K` the
/// C an octave up, on to `'` (F). A key's index is its semitone above the
/// keyboard's lowest note.
const NOTE_KEYS: [Key; 18] = [
    Key::A,
    Key::W,
    Key::S,
    Key::E,
    Key::D,
    Key::F,
    Key::T,
    Key::G,
    Key::Y,
    Key::H,
    Key::U,
    Key::J,
    Key::K,
    Key::O,
    Key::L,
    Key::P,
    Key::Semicolon,
    Key::Quote,
];

/// The velocities `C` / `V` step through, Ableton's steps of 20.
const VELOCITIES: [u8; 8] = [1, 20, 40, 60, 80, 100, 120, 127];

/// The velocity the keyboard starts at (`VELOCITIES[5]`, 100).
const DEFAULT_VELOCITY_IDX: usize = 5;

/// The lowest note the keyboard starts on: C3 (MIDI 60, Cubase-style
/// numbering, as the piano roll's).
const DEFAULT_LOWEST_NOTE: u8 = 60;

/// The highest the lowest note may go: C8, so `A` always plays something.
const MAX_LOWEST_NOTE: u8 = 120;

/// MIDI note-on status, channel 0 — the router rewrites the channel.
const NOTE_ON: u8 = 0x90;

/// MIDI note-off status, channel 0.
const NOTE_OFF: u8 = 0x80;

/// What a taken key does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyRole {
    /// Plays the note this many semitones above the lowest one.
    Note(u8),
    /// `Z`: the whole keyboard an octave down.
    OctaveDown,
    /// `X`: an octave up.
    OctaveUp,
    /// `C`: one velocity step down.
    VelocityDown,
    /// `V`: one velocity step up.
    VelocityUp,
}

/// The role of physical key `key`, `None` for a key the keyboard leaves alone.
fn key_role(key: Key) -> Option<KeyRole> {
    if let Some(idx) = NOTE_KEYS.iter().position(|&note_key| note_key == key) {
        return Some(KeyRole::Note(idx as u8));
    }
    match key {
        Key::Z => Some(KeyRole::OctaveDown),
        Key::X => Some(KeyRole::OctaveUp),
        Key::C => Some(KeyRole::VelocityDown),
        Key::V => Some(KeyRole::VelocityUp),
        _ => None,
    }
}

/// The physical key of an egui key event the keyboard takes while it is on,
/// `None` for one that goes on to the usual bindings. A press is taken
/// plain or with Shift; with ⌘/Ctrl or ⌥ it stays a chord (`⌘Z` is still
/// undo). A release is taken whatever the modifiers, so a key let go while
/// ⌘ is down still ends its note.
pub(crate) fn takes_key_event(
    physical_key: Option<Key>,
    pressed: bool,
    modifiers: Modifiers,
) -> Option<Key> {
    let key = physical_key?;
    key_role(key)?;
    let modifiers = KeyModifiers::from(modifiers);
    let chord = modifiers.command || modifiers.ctrl || modifiers.alt;
    (!pressed || !chord).then_some(key)
}

/// Whether the keyboard, while it plays, takes the press `chord` is before
/// the keymap sees it — a layout key, plain or with Shift (see
/// [`takes_key_event`]). The keymap's chords hold logical keys and the
/// keyboard takes physical ones, so this is exact on a US layout and a close
/// guess elsewhere: the Keyboard tab's note on a row, and the recorder's
/// refusal of a bare layout key for the toggle itself.
pub(crate) fn shadows(chord: Chord) -> bool {
    key_role(chord.key).is_some() && !chord.command && !chord.alt && !chord.ctrl
}

/// The keyboard's state: on or off, where it sits, how hard it plays, and
/// which notes its keys are holding.
#[derive(Debug)]
pub(crate) struct ComputerKeyboard {
    /// Whether the keys play notes.
    enabled: bool,
    /// The note `A` plays.
    lowest_note: u8,
    /// Index into [`VELOCITIES`] of the velocity a note-on carries.
    velocity_idx: usize,
    /// The note each of [`NOTE_KEYS`] is holding, by index. Kept per key, not
    /// recomputed at release, so a key held across an octave change still
    /// ends the note it started.
    held: [Option<u8>; NOTE_KEYS.len()],
}

impl Default for ComputerKeyboard {
    fn default() -> Self {
        ComputerKeyboard {
            enabled: false,
            lowest_note: DEFAULT_LOWEST_NOTE,
            velocity_idx: DEFAULT_VELOCITY_IDX,
            held: [None; NOTE_KEYS.len()],
        }
    }
}

impl ComputerKeyboard {
    /// Whether the keys play notes.
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns the keyboard on or off. The notes held when it turns off are
    /// the caller's to end ([`release_all`](Self::release_all)).
    pub(crate) fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    /// A taken key went down: the note-on it plays, if any. An octave or
    /// velocity key only moves the keyboard; a note key already holding a
    /// note (an OS key repeat) plays nothing new, and nor does one past
    /// MIDI's top note.
    pub(crate) fn press(&mut self, key: Key) -> Option<Midi3> {
        match key_role(key)? {
            KeyRole::Note(semitone) => {
                let slot = &mut self.held[semitone as usize];
                if slot.is_some() {
                    return None;
                }
                let note = self
                    .lowest_note
                    .checked_add(semitone)
                    .filter(|&n| n <= 127)?;
                *slot = Some(note);
                Some([NOTE_ON, note, VELOCITIES[self.velocity_idx]])
            }
            KeyRole::OctaveDown => {
                self.lowest_note = self.lowest_note.saturating_sub(12);
                None
            }
            KeyRole::OctaveUp => {
                self.lowest_note = (self.lowest_note + 12).min(MAX_LOWEST_NOTE);
                None
            }
            KeyRole::VelocityDown => {
                self.velocity_idx = self.velocity_idx.saturating_sub(1);
                None
            }
            KeyRole::VelocityUp => {
                self.velocity_idx = (self.velocity_idx + 1).min(VELOCITIES.len() - 1);
                None
            }
        }
    }

    /// A taken key came up: the note-off for the note it was holding, if any.
    pub(crate) fn release(&mut self, key: Key) -> Option<Midi3> {
        match key_role(key)? {
            KeyRole::Note(semitone) => self.held[semitone as usize]
                .take()
                .map(|note| [NOTE_OFF, note, 0]),
            _ => None,
        }
    }

    /// Ends every held note — the keys stopped being the keyboard's (it was
    /// turned off, a text field or overlay took the keyboard, the window lost
    /// focus), so their releases may never arrive. Returns the note-offs.
    pub(crate) fn release_all(&mut self) -> Vec<Midi3> {
        self.held
            .iter_mut()
            .filter_map(|slot| slot.take().map(|note| [NOTE_OFF, note, 0]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> ComputerKeyboard {
        let mut keyboard = ComputerKeyboard::default();
        keyboard.toggle();
        keyboard
    }

    #[test]
    fn the_home_row_plays_white_keys_from_c3() {
        let mut keyboard = on();
        assert_eq!(keyboard.press(Key::A), Some([NOTE_ON, 60, 100]));
        assert_eq!(keyboard.press(Key::W), Some([NOTE_ON, 61, 100]));
        assert_eq!(keyboard.press(Key::K), Some([NOTE_ON, 72, 100]));
        assert_eq!(keyboard.press(Key::Quote), Some([NOTE_ON, 77, 100]));
    }

    #[test]
    fn a_release_ends_the_note_its_press_started() {
        let mut keyboard = on();
        keyboard.press(Key::S);
        assert_eq!(keyboard.release(Key::S), Some([NOTE_OFF, 62, 0]));
        assert_eq!(keyboard.release(Key::S), None, "only once");
    }

    #[test]
    fn a_held_key_ends_its_own_note_across_an_octave_change() {
        let mut keyboard = on();
        keyboard.press(Key::A);
        keyboard.press(Key::X);
        assert_eq!(keyboard.press(Key::S), Some([NOTE_ON, 74, 100]));
        assert_eq!(keyboard.release(Key::A), Some([NOTE_OFF, 60, 0]));
    }

    #[test]
    fn a_repeated_press_while_held_plays_nothing_new() {
        let mut keyboard = on();
        keyboard.press(Key::D);
        assert_eq!(keyboard.press(Key::D), None);
    }

    #[test]
    fn the_octave_stays_within_midi() {
        let mut keyboard = on();
        for _ in 0..20 {
            keyboard.press(Key::Z);
        }
        assert_eq!(keyboard.press(Key::A), Some([NOTE_ON, 0, 100]), "C-2");
        for _ in 0..20 {
            keyboard.press(Key::X);
        }
        // C8 at the bottom; G8 (127) is the top, the keys above it play
        // nothing.
        assert_eq!(keyboard.press(Key::S), Some([NOTE_ON, 122, 100]));
        assert_eq!(keyboard.press(Key::G), Some([NOTE_ON, 127, 100]));
        assert_eq!(keyboard.press(Key::Y), None);
        assert_eq!(keyboard.release(Key::Y), None);
    }

    #[test]
    fn velocity_steps_and_clamps() {
        let mut keyboard = on();
        keyboard.press(Key::V);
        assert_eq!(keyboard.press(Key::A), Some([NOTE_ON, 60, 120]));
        for _ in 0..10 {
            keyboard.press(Key::V);
        }
        assert_eq!(keyboard.press(Key::S), Some([NOTE_ON, 62, 127]));
        for _ in 0..10 {
            keyboard.press(Key::C);
        }
        assert_eq!(keyboard.press(Key::D), Some([NOTE_ON, 64, 1]));
    }

    #[test]
    fn release_all_ends_every_held_note_once() {
        let mut keyboard = on();
        keyboard.press(Key::A);
        keyboard.press(Key::G);
        assert_eq!(
            keyboard.release_all(),
            vec![[NOTE_OFF, 60, 0], [NOTE_OFF, 67, 0]]
        );
        assert!(keyboard.release_all().is_empty());
        assert_eq!(keyboard.release(Key::A), None);
    }

    #[test]
    fn a_press_with_a_command_modifier_stays_a_chord() {
        let command = Modifiers::COMMAND;
        assert_eq!(takes_key_event(Some(Key::Z), true, command), None);
        assert_eq!(takes_key_event(Some(Key::A), true, Modifiers::ALT), None);
        assert_eq!(
            takes_key_event(Some(Key::A), true, Modifiers::SHIFT),
            Some(Key::A)
        );
        assert_eq!(
            takes_key_event(Some(Key::A), true, Modifiers::NONE),
            Some(Key::A)
        );
    }

    #[test]
    fn a_release_is_taken_whatever_the_modifiers() {
        assert_eq!(
            takes_key_event(Some(Key::A), false, Modifiers::COMMAND),
            Some(Key::A)
        );
    }

    #[test]
    fn other_keys_are_left_alone() {
        for key in [
            Key::Space,
            Key::Slash,
            Key::Num0,
            Key::Q,
            Key::M,
            Key::Enter,
        ] {
            assert_eq!(takes_key_event(Some(key), true, Modifiers::NONE), None);
        }
        assert_eq!(takes_key_event(None, true, Modifiers::NONE), None);
    }

    #[test]
    fn a_layout_key_shadows_its_plain_and_shifted_chords_only() {
        let t = Chord {
            key: Key::T,
            command: false,
            alt: false,
            shift: false,
            ctrl: false,
        };
        assert!(shadows(t));
        assert!(shadows(Chord { shift: true, ..t }));
        assert!(!shadows(Chord { command: true, ..t }));
        assert!(!shadows(Chord { alt: true, ..t }));
        assert!(!shadows(Chord { key: Key::Q, ..t }));
        assert!(shadows(Chord { key: Key::Z, ..t }));
    }
}
