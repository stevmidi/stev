//! Building a clip from a MIDI file's events — the model half of the MIDI
//! clip import (`060-persistence.md` § MIDI clip import). The file is read by
//! `core::project::read_smf`; this turns what it found into a clip that
//! loops cleanly.

use crate::{
    core::time::Meter,
    models::event::{Event, EventType},
};

use super::Clip;

impl Clip {
    /// A clip of `events` (sorted, ticks from the file start), its window
    /// `0..length` where `length` is `end_tick` (the file's end-of-track) or
    /// the last event, if later, rounded up to whole bars of `meter` — so it loops on
    /// the bar like a capture does. A note still open at the end is closed
    /// there; a `NoteOff` with no note to close is dropped. `None` when there
    /// is no note at all: an empty clip is nothing to import.
    pub(crate) fn imported(events: Vec<Event>, end_tick: i32, meter: Meter) -> Option<Clip> {
        let last_note_on = events
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(Event::tick)
            .max()?;
        let last_event = events.last().map_or(0, Event::tick);
        let content_end = end_tick.max(last_event).max(last_note_on + 1);
        // Round up to whole bars: the first bar line at or after the end.
        let length = meter.next_bar_boundary_after(content_end - 1);

        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(length));
        clip.events = events;
        clip.sort_events_by_tick();
        clip.remove_orphan_note_offs();
        clip.close_open_notes_at(length);
        clip.calculate_note_lengths();
        Some(clip)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::time::{PPQN, bars_to_ticks};

    use super::*;

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    /// `(tick, message)` of every event.
    fn events(clip: &Clip) -> Vec<(i32, Vec<u8>)> {
        clip.events()
            .iter()
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect()
    }

    #[test]
    fn no_notes_is_nothing_to_import() {
        assert!(Clip::imported(vec![], PPQN * 4, Meter::FOUR_FOUR).is_none());
        let cc_only = vec![Event::new(0, 0, vec![0xB0, 64, 127])];
        assert!(Clip::imported(cc_only, PPQN * 4, Meter::FOUR_FOUR).is_none());
    }

    #[test]
    fn length_is_the_end_of_track_rounded_up_to_whole_bars() {
        let bar = bars_to_ticks(1);
        let notes = || vec![on(0, 60), off(PPQN, 60)];

        let clip = Clip::imported(notes(), bar, Meter::FOUR_FOUR).unwrap();
        assert_eq!((clip.region().start(), clip.region().end()), (0, bar));

        let clip = Clip::imported(notes(), bar + 1, Meter::FOUR_FOUR).unwrap();
        assert_eq!(clip.region_length(), bar * 2);

        // An end-of-track before the last note-off: the note-off wins.
        let clip = Clip::imported(notes(), 0, Meter::FOUR_FOUR).unwrap();
        assert_eq!(clip.region_length(), bar);
    }

    #[test]
    fn a_note_on_a_bar_line_at_the_end_gets_its_bar() {
        let bar = bars_to_ticks(1);
        let clip = Clip::imported(vec![on(bar, 60), off(bar, 60)], bar, Meter::FOUR_FOUR).unwrap();
        assert_eq!(clip.region_length(), bar * 2);
    }

    /// The length rounds up to whole bars of the project's meter.
    #[test]
    fn length_rounds_up_to_bars_of_the_meter() {
        let seven_eight = Meter::new(7, 8).unwrap();
        let bar = seven_eight.bar_ticks();
        let clip = Clip::imported(vec![on(0, 60), off(PPQN, 60)], bar + 1, seven_eight).unwrap();
        assert_eq!(clip.region_length(), bar * 2);
    }

    #[test]
    fn open_notes_close_at_the_end_and_orphan_offs_are_dropped() {
        let bar = bars_to_ticks(1);
        let clip = Clip::imported(
            vec![off(0, 62), on(PPQN, 61), on(PPQN, 60)],
            bar,
            Meter::FOUR_FOUR,
        )
        .unwrap();
        let mut got = events(&clip);
        got.sort();
        assert_eq!(
            got,
            vec![
                (PPQN, vec![0x90, 60, 100]),
                (PPQN, vec![0x90, 61, 100]),
                (bar, vec![0x80, 60, 0]),
                (bar, vec![0x80, 61, 0]),
            ]
        );
        assert!(
            clip.events()
                .iter()
                .filter(|e| e.event_type() == Some(EventType::NoteOn))
                .all(|e| e.end_tick() == bar)
        );
    }

    #[test]
    fn note_lengths_are_calculated_and_other_messages_kept() {
        let cc = Event::new(PPQN / 2, 0, vec![0xB0, 64, 127]);
        let clip = Clip::imported(vec![on(0, 60), cc, off(PPQN, 60)], 0, Meter::FOUR_FOUR).unwrap();
        assert_eq!(clip.events()[0].end_tick(), PPQN);
        assert_eq!(clip.events().len(), 3);
    }
}
