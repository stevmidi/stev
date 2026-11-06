//! Note ids, for the formats whose notes carry them.
//!
//! [`NoteIds`] pairs each sounding note-on with the id its note-off must repeat.
//! Each [`NoteSource`] holds its own notes. A live key over a clip's note of
//! the same pitch is two notes with two ids, each ended only by its own off, so
//! a plugin keying voices by id plays both, and neither cuts the other. One
//! source holds one note per key: a second on from it takes the key over, and
//! the old note gets its off first ([`displaced_note_off`](NoteIds::displaced_note_off)),
//! so no id is ever left without one and a source's single off closes its key.
//!
//! "No note here" is `None`, and each format decides what that means: `-1` is
//! VST3's "no id", but in CLAP it's a wildcard matching every note on the key.
//!
//! See `130-plugin-host.md` and `docs/180-vst3-host.md`.

use crate::core::midi::message::{Midi3, parse_note};
use crate::core::sequencer::NoteSource;

/// MIDI status nibble for note-off.
const NOTE_OFF: u8 = 0x80;
/// MIDI channels, and keys per channel — the shape of the note-id table.
pub(super) const CHANNELS: usize = 16;
/// Keys per MIDI channel.
const KEYS: usize = 128;
/// [`NoteSource`]s, the last one's index plus one.
const SOURCES: usize = NoteSource::Audition as usize + 1;

/// Pairs each sounding note-on with the id its note-off must repeat.
///
/// A fresh id is minted per note-on and remembered until the matching off from
/// the same source. One id per source and key: a second on from it with no off
/// in between first releases the note it displaces
/// ([`displaced_note_off`](Self::displaced_note_off)).
pub(super) struct NoteIds {
    /// `active[source][channel][key]` is the id of the note-on still awaiting
    /// its off. Boxed because it is ~48 KB and only read through a reference.
    active: Box<[[[Option<i32>; KEYS]; CHANNELS]; SOURCES]>,
    /// Next id to hand out, never negative. Wraps rather than overflowing; ids
    /// only have to be unique among *simultaneously sounding* notes, and
    /// `i32::MAX` of them is not a situation that arises.
    next: i32,
}

impl Default for NoteIds {
    fn default() -> Self {
        Self {
            active: Box::new([[[None; KEYS]; CHANNELS]; SOURCES]),
            next: 0,
        }
    }
}

impl NoteIds {
    /// Mints and stores the id for a note-on.
    pub(super) fn begin(&mut self, source: NoteSource, channel: usize, key: usize) -> i32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(0);
        self.active[source as usize][channel][key] = Some(id);
        id
    }

    /// Takes the id a note-off should carry. `None` when no matching on is
    /// outstanding: a stray off, such as the second off after a retrigger.
    pub(super) fn end(&mut self, source: NoteSource, channel: usize, key: usize) -> Option<i32> {
        self.active[source as usize][channel][key].take()
    }

    /// The id of the note currently sounding on a key, *without* releasing it —
    /// what a poly-pressure message needs so the plugin knows which voice it
    /// refers to.
    pub(super) fn current(&self, source: NoteSource, channel: usize, key: usize) -> Option<i32> {
        self.active[source as usize][channel][key]
    }

    /// The note-off for the note `bytes` is about to displace, if `bytes` is a
    /// note-on over a key its `source` still has sounding: two overlapping
    /// notes of one pitch in a clip, or two quick auditions. Send it ahead of
    /// the note-on.
    ///
    /// Overwriting the id instead would leave the old note with no off at all,
    /// and a plugin keying voices by id would hold it forever. Another source's
    /// note on the key is left alone.
    pub(super) fn displaced_note_off(&self, bytes: Midi3, source: NoteSource) -> Option<Midi3> {
        let Some((key, true)) = parse_note(&bytes) else {
            return None;
        };
        let channel = bytes[0] & 0x0F;
        self.current(source, usize::from(channel), key)?;
        Some([NOTE_OFF | channel, bytes[1], 0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_off_takes_its_ons_id_once() {
        let mut ids = NoteIds::default();
        let id = ids.begin(NoteSource::Clip, 0, 60);
        assert_eq!(ids.end(NoteSource::Clip, 0, 60), Some(id));
        assert_eq!(ids.end(NoteSource::Clip, 0, 60), None);
    }

    #[test]
    fn sources_hold_a_key_apart() {
        let mut ids = NoteIds::default();
        let clip = ids.begin(NoteSource::Clip, 0, 60);
        let live = ids.begin(NoteSource::Live, 0, 60);
        assert_ne!(clip, live);
        assert_eq!(ids.end(NoteSource::Clip, 0, 60), Some(clip));
        assert_eq!(ids.current(NoteSource::Live, 0, 60), Some(live));
    }

    #[test]
    fn ids_stay_non_negative_past_the_wrap() {
        // CLAP reads a negative id as a wildcard.
        let mut ids = NoteIds {
            next: i32::MAX,
            ..NoteIds::default()
        };
        assert_eq!(ids.begin(NoteSource::Clip, 0, 60), i32::MAX);
        assert_eq!(ids.begin(NoteSource::Clip, 0, 61), 0);
    }

    #[test]
    fn a_note_on_displaces_only_its_own_sources_note() {
        let mut ids = NoteIds::default();
        ids.begin(NoteSource::Clip, 2, 60);
        let on = [0x92, 60, 100];
        assert_eq!(
            ids.displaced_note_off(on, NoteSource::Clip),
            Some([0x82, 60, 0])
        );
        assert_eq!(ids.displaced_note_off(on, NoteSource::Live), None);
    }

    #[test]
    fn only_a_note_on_displaces() {
        let mut ids = NoteIds::default();
        ids.begin(NoteSource::Clip, 0, 60);
        assert_eq!(
            ids.displaced_note_off([0x80, 60, 0], NoteSource::Clip),
            None
        );
        assert_eq!(
            ids.displaced_note_off([0x90, 60, 0], NoteSource::Clip),
            None
        );
    }
}
