//! Baking clips into what they play — the model half of Merge Clips
//! (`⌘/Ctrl+J`, `PasteClipsEdit::merging`) and of the MIDI clip export
//! (`⌘/Ctrl+⇧+E`). Only what plays is carried over, so the merged clip sounds
//! exactly like the clips it replaces, and an exported file like the clip.

use std::collections::HashMap;

use uuid::Uuid;

use crate::core::midi::message::Wheel;
use crate::models::{event::Event, wheels::apply_wheel_move};

use super::Clip;

/// What [`Clip::merged`] built.
pub(crate) struct MergedClip {
    /// The merged clip: window `0..length`, at `start_tick` 0, fresh event ids.
    pub(crate) clip: Clip,
    /// Whether every source event came over unaltered — none hidden outside
    /// a window, no note shortened or closed, none muted by a muted clip. A
    /// lossless bake of one clip that already spans the range changes
    /// nothing, so the edit skips it.
    pub(crate) lossless: bool,
}

impl Clip {
    /// Bakes `pieces` into one clip of `length` ticks. Each piece's window is
    /// laid down at the piece's `start_tick` (relative to the merged clip's
    /// start), so gaps between pieces become silence. Per piece, only what
    /// plays survives: a note whose `NoteOn` lies in the window, its end
    /// stopped at the window end (playback releases it there today, so a
    /// merge never lets it ring on; an open note ends there too); other
    /// events inside the window as they are, plus the wheel moves playback
    /// makes at the window's edges ([`wheel_edges`](Self::wheel_edges)).
    /// Hidden material outside a window is dropped. Every event of a muted
    /// piece comes over muted, so nothing new sounds and nothing is lost.
    /// The swing is the earliest piece's.
    pub(crate) fn merged(pieces: &[&Clip], length: i32) -> MergedClip {
        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(length));

        let mut lossless = true;
        for piece in pieces {
            lossless &= piece.bake_into(&mut clip.events);
        }
        if let Some(first) = pieces.iter().min_by_key(|piece| piece.start_tick()) {
            clip.set_swing_pct(first.swing_pct());
        }

        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        MergedClip { clip, lossless }
    }

    /// What a MIDI clip export writes: the events this clip plays, timed from
    /// its window start (the file starts where the clip does). The
    /// [`merged`](Self::merged) rules — only what is inside the window, notes
    /// stopped at the window end, the wheel moves at its edges, hidden
    /// material dropped — minus muted events. The clip's own mute is ignored:
    /// exporting a clip is asking for its content. Sorted, offs first on a
    /// shared tick.
    pub(crate) fn exported_events(&self) -> Vec<Event> {
        let (window_start, window_end) = (self.region.start(), self.region.end());
        let length = window_end - window_start;
        let kept =
            self.windowed_events(window_start..window_end, |tick| tick - window_start, length);
        let mut export = Clip::new();
        export.events = self.wheel_edges(&kept, 0, length);
        export
            .events
            .extend(kept.into_iter().filter(|event| !event.is_muted()));
        export.sort_events_by_tick();
        export.events
    }

    /// Appends what this clip plays to `out` (see [`merged`](Self::merged)),
    /// retimed so the window start lands on `start_tick`, with fresh ids.
    /// Returns whether every event came over unaltered: same events, same
    /// ticks, none muted by the clip.
    fn bake_into(&self, out: &mut Vec<Event>) -> bool {
        let (window_start, window_end) = (self.region.start(), self.region.end());
        let offset = self.start_tick - window_start;
        let kept = self.windowed_events(
            window_start..window_end,
            |tick| tick + offset,
            window_end + offset,
        );

        // Kept clones carry their source ids; a closed note's synthetic off
        // has a fresh one, so it never matches.
        let source: HashMap<Uuid, i32> = self
            .events
            .iter()
            .map(|event| (event.id(), event.tick() + offset))
            .collect();
        let edges = self.wheel_edges(&kept, window_start + offset, window_end + offset);
        let lossless = !self.muted
            && edges.is_empty()
            && kept.len() == self.events.len()
            && kept
                .iter()
                .all(|event| source.get(&event.id()) == Some(&event.tick()));

        // Edge moves first: one at the window start goes ahead of the
        // window's own events on that tick, as playback sends its chase.
        out.extend(edges.into_iter().chain(kept).map(|event| {
            let muted = event.is_muted() || self.muted;
            let mut copy = Event::new(event.tick(), 0, event.into_midi_message());
            copy.set_muted(muted);
            copy
        }));
        lossless
    }

    /// The wheel moves playback makes around this clip's window that no
    /// event in it holds, so a bake sounds as the clip did: at `start_tick`
    /// the value of a wheel held from hidden material before the window (the
    /// track's arrival chase, `Track::tick`), and at `end_tick` neutral for a
    /// wheel the window leaves off it (the chase when playback moves on,
    /// `Track::seek`). `kept` is the window's events
    /// ([`windowed_events`](Self::windowed_events)); muted ones never sound,
    /// so they move nothing.
    fn wheel_edges(&self, kept: &[Event], start_tick: i32, end_tick: i32) -> Vec<Event> {
        let held = self.wheels_at(self.start_tick);
        let mut at_end = held;
        for event in kept {
            apply_wheel_move(&mut at_end, event);
        }

        let (mut starts, mut ends) = (Vec::new(), Vec::new());
        for wheel in Wheel::ALL {
            let (neutral, i) = (wheel.neutral(), wheel.index());
            if held[i] != neutral {
                starts.push(Event::new(start_tick, 0, wheel.message(held[i])));
            }
            if at_end[i] != neutral {
                ends.push(Event::new(end_tick, 0, wheel.message(neutral)));
            }
        }
        starts.extend(ends);
        starts
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{
        clip::Clip,
        event::{Event, EventType},
    };

    fn on(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x90, note, 100])
    }

    fn off(tick: i32, note: u8) -> Event {
        Event::new(tick, 0, vec![0x80, note, 0])
    }

    /// A clip at `start_tick` with window `[region_start, region_end)` holding
    /// the notes `(on, off, pitch)`.
    fn piece(start_tick: i32, region: (i32, i32), notes: &[(i32, i32, u8)]) -> Clip {
        let mut clip = Clip::new();
        clip.set_start_tick(start_tick);
        clip.region_mut().set_region(Some(region.0), Some(region.1));
        for &(start, end, pitch) in notes {
            clip.add_event(on(start, pitch));
            clip.add_event(off(end, pitch));
        }
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip
    }

    /// `(start, length, pitch, muted)` of every note.
    fn notes(clip: &Clip) -> Vec<(i32, i32, u8, bool)> {
        clip.events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .map(|e| {
                (
                    e.tick(),
                    e.end_tick() - e.tick(),
                    e.note_number().unwrap(),
                    e.is_muted(),
                )
            })
            .collect()
    }

    #[test]
    fn lays_each_window_at_its_piece_start_with_gaps_as_silence() {
        let a = piece(0, (0, 480), &[(0, 240, 60)]);
        // Window starts at 960 in event space; placed at 960 in the merge.
        let b = piece(960, (960, 1440), &[(1000, 1100, 62)]);

        let merged = Clip::merged(&[&a, &b], 1920);

        assert_eq!(merged.clip.start_tick(), 0);
        assert_eq!(
            (merged.clip.region().start(), merged.clip.region().end()),
            (0, 1920)
        );
        assert_eq!(
            notes(&merged.clip),
            vec![(0, 240, 60, false), (1000, 100, 62, false)]
        );
        assert!(merged.lossless);
    }

    #[test]
    fn retimes_a_window_that_starts_inside_event_space() {
        // A split-off right half: window [480, 960) sits at 0 in the merge.
        let right = piece(0, (480, 960), &[(600, 700, 60)]);
        let merged = Clip::merged(&[&right], 480);
        assert_eq!(notes(&merged.clip), vec![(120, 100, 60, false)]);
    }

    #[test]
    fn drops_hidden_material_and_stops_notes_at_the_window_end() {
        let clip = piece(
            0,
            (240, 720),
            &[
                (0, 100, 59),   // before the window — hidden
                (200, 400, 60), // starts before the window — never plays
                (300, 400, 61), // inside
                (600, 900, 62), // runs past the end — cut at 720 today
                (800, 900, 63), // after the window — hidden
            ],
        );

        let merged = Clip::merged(&[&clip], 480);

        assert_eq!(
            notes(&merged.clip),
            vec![(60, 100, 61, false), (360, 120, 62, false)]
        );
        assert!(!merged.lossless);
    }

    #[test]
    fn notes_of_a_muted_piece_come_over_muted() {
        let mut muted = piece(0, (0, 480), &[(0, 100, 60)]);
        muted.set_muted(true);
        let loud = piece(480, (0, 480), &[(0, 100, 62)]);

        let merged = Clip::merged(&[&muted, &loud], 960);

        assert!(!merged.clip.is_muted());
        assert_eq!(
            notes(&merged.clip),
            vec![(0, 100, 60, true), (480, 100, 62, false)]
        );
        assert!(!merged.lossless, "muting notes changes the clip");
    }

    #[test]
    fn the_wheels_of_a_muted_piece_come_over_muted() {
        let mut muted = piece(0, (0, 480), &[(0, 100, 60)]);
        muted.add_event(Event::new(50, 0, vec![0xE0, 0x00, 0x60]));
        muted.add_event(Event::new(90, 0, vec![0xE0, 0x00, 0x40]));
        muted.sort_events_by_tick();
        muted.set_muted(true);

        let merged = Clip::merged(&[&muted], 480);

        assert!(merged.clip.events().iter().all(Event::is_muted));
    }

    /// `(tick, message)` of every wheel event in `events`.
    fn wheel_moves(events: &[Event]) -> Vec<(i32, Vec<u8>)> {
        events
            .iter()
            .filter(|e| e.event_type().is_none())
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect()
    }

    /// Playback chases a bend held from before a window and resets it where
    /// the clip ends; the merge writes both down, so the merged clip — one
    /// clip, no boundary in between — sounds the same.
    #[test]
    fn a_merge_keeps_the_wheel_moves_playback_makes_at_a_windows_edges() {
        // Bent up before the window (hidden), never back: playback sends the
        // bend on arriving at 240 and centres it at the clip's end.
        let mut bent = piece(0, (240, 720), &[(300, 400, 61)]);
        bent.add_event(Event::new(100, 0, vec![0xE0, 0x00, 0x60]));
        bent.sort_events_by_tick();
        // The next clip starts with its mod wheel up and leaves it there.
        let mut modded = piece(480, (0, 480), &[(0, 100, 62)]);
        modded.add_event(Event::new(0, 0, vec![0xB0, 0x01, 90]));
        modded.sort_events_by_tick();

        let merged = Clip::merged(&[&bent, &modded], 960);

        assert_eq!(
            wheel_moves(merged.clip.events()),
            vec![
                (0, vec![0xE0, 0x00, 0x60]),
                (480, vec![0xE0, 0x00, 0x40]),
                (480, vec![0xB0, 0x01, 90]),
                (960, vec![0xB0, 0x01, 0]),
            ]
        );
        assert!(!merged.lossless);
    }

    #[test]
    fn a_clip_that_leaves_its_wheels_at_rest_merges_losslessly() {
        let mut clip = piece(0, (0, 480), &[(0, 100, 60)]);
        clip.add_event(Event::new(50, 0, vec![0xE0, 0x00, 0x60]));
        clip.add_event(Event::new(90, 0, vec![0xE0, 0x00, 0x40]));
        clip.sort_events_by_tick();

        let merged = Clip::merged(&[&clip], 480);

        assert_eq!(wheel_moves(merged.clip.events()).len(), 2);
        assert!(merged.lossless);
    }

    /// A note held across a split stops at the split and isn't restarted in
    /// the right half, so the merge keeps it short — it sounds as before.
    #[test]
    fn a_note_cut_by_a_split_stays_cut() {
        let left = piece(0, (0, 480), &[(400, 600, 60)]);
        let right = piece(480, (480, 960), &[(400, 600, 60)]);

        let merged = Clip::merged(&[&left, &right], 960);

        assert_eq!(notes(&merged.clip), vec![(400, 80, 60, false)]);
    }

    /// A same-pitch note ending exactly where the next piece's starts pairs
    /// with its own off, not the next note's.
    #[test]
    fn same_pitch_notes_meeting_at_a_seam_keep_their_lengths() {
        let a = piece(0, (0, 480), &[(240, 480, 60)]);
        let b = piece(480, (0, 480), &[(0, 240, 60)]);

        let merged = Clip::merged(&[&a, &b], 960);

        assert_eq!(
            notes(&merged.clip),
            vec![(240, 240, 60, false), (480, 240, 60, false)]
        );
    }

    #[test]
    fn takes_the_earliest_pieces_swing_and_fresh_event_ids() {
        let mut late = piece(480, (0, 480), &[(0, 100, 62)]);
        late.set_swing_pct(70);
        let mut early = piece(0, (0, 480), &[(0, 100, 60)]);
        early.set_swing_pct(58);

        let merged = Clip::merged(&[&late, &early], 960);

        assert_eq!(merged.clip.swing_pct(), 58);
        let source_ids: Vec<_> = early.events().iter().map(Event::id).collect();
        assert!(
            merged
                .clip
                .events()
                .iter()
                .all(|e| !source_ids.contains(&e.id()))
        );
    }

    /// `(tick, status, note)` of every exported event.
    fn exported(clip: &Clip) -> Vec<(i32, u8, u8)> {
        clip.exported_events()
            .iter()
            .map(|e| (e.tick(), e.midi_message()[0], e.midi_message()[1]))
            .collect()
    }

    #[test]
    fn export_times_events_from_the_window_start_and_drops_hidden_material() {
        let clip = piece(
            960,
            (240, 720),
            &[
                (0, 100, 59),   // before the window — hidden
                (300, 400, 61), // inside
                (600, 900, 62), // runs past the end — stopped at it
                (800, 900, 63), // after the window — hidden
            ],
        );

        assert_eq!(
            exported(&clip),
            vec![
                (60, 0x90, 61),
                (160, 0x80, 61),
                (360, 0x90, 62),
                (480, 0x80, 62)
            ]
        );
    }

    #[test]
    fn export_skips_muted_notes_but_not_a_muted_clip() {
        let mut clip = piece(0, (0, 960), &[(0, 100, 60), (480, 580, 62)]);
        clip.set_muted(true);
        let muted_on = clip.events()[0].id();
        clip.select_event(Some(muted_on));
        clip.toggle_muted_for_selected_events();

        assert_eq!(exported(&clip), vec![(480, 0x90, 62), (580, 0x80, 62)]);
    }

    #[test]
    fn export_writes_a_held_bend_at_the_start_and_centres_it_at_the_end() {
        let mut clip = piece(0, (240, 720), &[(300, 400, 61)]);
        clip.add_event(Event::new(100, 0, vec![0xE0, 0x00, 0x60]));
        clip.sort_events_by_tick();

        assert_eq!(
            wheel_moves(&clip.exported_events()),
            vec![(0, vec![0xE0, 0x00, 0x60]), (480, vec![0xE0, 0x00, 0x40])]
        );
    }

    #[test]
    fn export_puts_a_note_off_ahead_of_a_same_tick_note_on() {
        let clip = piece(0, (0, 960), &[(0, 480, 60), (480, 960, 60)]);
        assert_eq!(
            exported(&clip),
            vec![
                (0, 0x90, 60),
                (480, 0x80, 60),
                (480, 0x90, 60),
                (960, 0x80, 60)
            ]
        );
    }
}
