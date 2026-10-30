//! Landing a captured take's wheel moves in a clip — pitch bend and the mod
//! wheel, by the latch rule of `370-wheel-capture.md` § Decisions 5–7.
//!
//! A take's notes are added to the clip. Its wheels *replace*: for each
//! wheel the take moved, the clip's own data for that wheel goes from the
//! take's first move to the end of the take's window, the take's moves come
//! in, and at the window's end the value the clip's old curve had there is
//! written back, if it differs from the take's last. A wheel the take didn't
//! move is left alone. A wheel off its rest where a take starts counts as a
//! move there ([`carry_wheels_onto`](Clip::carry_wheels_onto)).

use crate::{
    core::midi::message::{Wheel, parse_wheel, rewrite_channel},
    models::{
        event::Event,
        wheels::{NEUTRAL_WHEELS, WheelValues, apply_wheel_move},
    },
};

use super::Clip;

impl Clip {
    /// The wheel values in force just before event tick `tick`: per wheel,
    /// the last unmuted move before it, or neutral when there is none.
    pub(crate) fn wheels_before(&self, tick: i32) -> WheelValues {
        let mut values = NEUTRAL_WHEELS;
        for event in self.events.iter().take_while(|event| event.tick() < tick) {
            apply_wheel_move(&mut values, event);
        }
        values
    }

    /// What a take starting at event tick `tick` carries: for each wheel off
    /// its rest there that doesn't move exactly there already, its last
    /// unmuted move before `tick` (value and channel), restamped at `tick`.
    pub(crate) fn carried_wheel_moves(&self, tick: i32) -> Vec<Event> {
        let mut last: [Option<(&Event, u16)>; Wheel::ALL.len()] = [None; Wheel::ALL.len()];
        for event in self.events.iter().take_while(|event| event.tick() < tick) {
            if let Some((wheel, value)) = parse_wheel(event.midi_message())
                && !event.is_muted()
            {
                last[wheel.index()] = Some((event, value));
            }
        }
        Wheel::ALL
            .into_iter()
            .filter_map(|wheel| {
                let (event, value) = last[wheel.index()]?;
                (value != wheel.neutral() && !self.has_wheel_move_at(wheel, tick))
                    .then(|| Event::from_midi_with_tick(event.midi_message(), tick))
            })
            .collect()
    }

    /// A take starting at event tick `tick` carries every wheel off its rest
    /// there ([`carried_wheel_moves`](Self::carried_wheel_moves)): the moves
    /// are added at `tick`.
    pub(crate) fn carry_wheels_onto(&mut self, tick: i32) {
        let carried = self.carried_wheel_moves(tick);
        if carried.is_empty() {
            return;
        }
        for event in carried {
            self.add_event(event);
        }
        self.sort_events_by_tick();
    }

    /// Lands `take` — a captured take in this clip's event ticks — by the
    /// latch rule (see the module docs). Its notes are added. For each wheel
    /// it moves before `wheel_end` (never past the window's end), the clip's
    /// own moves of that wheel from the take's first move up to `wheel_end`
    /// make way for the take's, and the old curve's value at `wheel_end` is
    /// written back there when it differs from the take's last and the clip
    /// doesn't move that wheel exactly there anyway — unless `wheel_end` is
    /// the window's end, where the clip loops and the chase at the wrap hands
    /// back. Take moves at or past `wheel_end` are dropped. Re-sorted and
    /// note lengths recomputed.
    pub(crate) fn land_take(&mut self, take: &[Event], wheel_end: i32) {
        let window_end = self.region().end();
        let wheel_end = wheel_end.min(window_end);
        let old_at_end = self.wheels_before(wheel_end);

        for wheel in Wheel::ALL {
            let moves: Vec<(&Event, u16)> = take
                .iter()
                .filter(|event| event.tick() < wheel_end)
                .filter_map(|event| match parse_wheel(event.midi_message()) {
                    Some((moved, value)) if moved == wheel => Some((event, value)),
                    _ => None,
                })
                .collect();
            let (Some(first), Some(&(last, last_value))) = (
                moves.iter().map(|(event, _)| event.tick()).min(),
                moves.iter().max_by_key(|(event, _)| event.tick()),
            ) else {
                continue;
            };
            let old = old_at_end[wheel.index()];
            let write_back = wheel_end < window_end
                && last_value != old
                && !self.has_wheel_move_at(wheel, wheel_end);

            self.events.retain(|event| {
                !(is_move_of(event, wheel) && (first..wheel_end).contains(&event.tick()))
            });

            if write_back {
                let mut message = wheel.message(old);
                rewrite_channel(&mut message, last.midi_channel().unwrap_or(0));
                self.add_event(Event::from_midi_with_tick(&message, wheel_end));
            }
        }

        for event in take {
            let is_late_wheel = !event.is_note_edge() && event.tick() >= wheel_end;
            if !is_late_wheel {
                self.add_event(event.clone());
            }
        }
        self.sort_events_by_tick();
        self.calculate_note_lengths();
    }

    /// Whether the clip holds a wheel move. A running take of these alone
    /// lands in the lead clip (`370-wheel-capture.md` § Decisions 3–4).
    pub(crate) fn has_wheel_moves(&self) -> bool {
        self.events
            .iter()
            .any(|event| parse_wheel(event.midi_message()).is_some())
    }

    /// Whether the clip has a move of `wheel` exactly at event tick `tick`.
    fn has_wheel_move_at(&self, wheel: Wheel, tick: i32) -> bool {
        self.events
            .iter()
            .any(|event| event.tick() == tick && is_move_of(event, wheel))
    }
}

/// Whether `event` moves `wheel`, muted or not.
fn is_move_of(event: &Event, wheel: Wheel) -> bool {
    parse_wheel(event.midi_message()).is_some_and(|(moved, _)| moved == wheel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::event::EventType;

    const BAR: i32 = 1920;

    fn bend(tick: i32, msb: u8) -> Event {
        Event::new(tick, 0, vec![0xE0, 0x00, msb])
    }

    fn modw(tick: i32, value: u8) -> Event {
        Event::new(tick, 0, vec![0xB0, 0x01, value])
    }

    fn clip_with(events: Vec<Event>) -> Clip {
        let mut clip = Clip::new();
        clip.restore_events(events);
        clip.region_mut().set_region(Some(0), Some(BAR * 4));
        clip
    }

    /// The clip's wheel moves as `(tick, message)`.
    fn wheel_moves(clip: &Clip) -> Vec<(i32, Vec<u8>)> {
        clip.events()
            .iter()
            .filter(|event| !event.is_note_edge())
            .map(|event| (event.tick(), event.midi_message().to_vec()))
            .collect()
    }

    /// A mod sweep across four bars: up a step every bar.
    fn swept() -> Clip {
        clip_with(
            (0..4)
                .map(|bar| modw(bar * BAR, 20 + bar as u8 * 20))
                .collect(),
        )
    }

    /// The walkthrough: a move halfway through bar 3 replaces the sweep from
    /// there to the end of bar 3, and the old curve's value comes back at
    /// bar 4.
    #[test]
    fn a_wheel_span_runs_from_its_first_move_to_the_window_end() {
        let mut clip = swept();

        clip.land_take(
            &[modw(BAR * 2 + 960, 100), modw(BAR * 2 + 1200, 110)],
            BAR * 3,
        );

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (BAR, vec![0xB0, 0x01, 40]),
                (BAR * 2, vec![0xB0, 0x01, 60]),
                (BAR * 2 + 960, vec![0xB0, 0x01, 100]),
                (BAR * 2 + 1200, vec![0xB0, 0x01, 110]),
                (BAR * 3, vec![0xB0, 0x01, 80]),
            ]
        );
    }

    /// The old curve's own move at the window end already hands back.
    #[test]
    fn no_write_back_where_the_clip_moves_the_wheel_at_the_end() {
        let mut clip = swept();

        clip.land_take(&[modw(BAR + 100, 100)], BAR * 2);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (BAR, vec![0xB0, 0x01, 40]),
                (BAR + 100, vec![0xB0, 0x01, 100]),
                (BAR * 2, vec![0xB0, 0x01, 60]),
                (BAR * 3, vec![0xB0, 0x01, 80]),
            ]
        );
    }

    /// A take ending on the old curve's value needs no write-back, and the
    /// clip's moves inside the span are replaced, not interleaved.
    #[test]
    fn no_write_back_when_the_take_ends_on_the_old_value() {
        let mut clip = clip_with(vec![bend(BAR, 0x60), bend(BAR + 500, 0x40)]);

        clip.land_take(&[bend(BAR + 200, 0x30), bend(BAR + 400, 0x40)], BAR * 2);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (BAR, vec![0xE0, 0x00, 0x60]),
                (BAR + 200, vec![0xE0, 0x00, 0x30]),
                (BAR + 400, vec![0xE0, 0x00, 0x40]),
            ]
        );
    }

    /// Riding one wheel leaves the other's curve alone.
    #[test]
    fn an_untouched_wheel_is_left_alone() {
        let mut clip = clip_with(vec![bend(100, 0x60), modw(200, 50), bend(900, 0x40)]);

        clip.land_take(&[modw(300, 90)], BAR);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (100, vec![0xE0, 0x00, 0x60]),
                (200, vec![0xB0, 0x01, 50]),
                (300, vec![0xB0, 0x01, 90]),
                (900, vec![0xE0, 0x00, 0x40]),
                (BAR, vec![0xB0, 0x01, 50]),
            ]
        );
    }

    /// A notes-only take touches neither wheel; its notes are added.
    #[test]
    fn a_notes_only_take_adds_its_notes_and_leaves_the_wheels() {
        let mut clip = clip_with(vec![bend(100, 0x60), modw(200, 50)]);
        let before = wheel_moves(&clip);

        clip.land_take(
            &[
                Event::new(300, 0, vec![0x90, 60, 100]),
                Event::new(600, 0, vec![0x80, 60, 0]),
            ],
            BAR,
        );

        assert_eq!(wheel_moves(&clip), before);
        let ons: Vec<_> = clip
            .events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| (event.tick(), event.end_tick()))
            .collect();
        assert_eq!(ons, vec![(300, 600)]);
    }

    /// A clip without a curve has its wheels at rest: the take's moves come
    /// in, and a wheel the take leaves off its rest goes back to rest at the
    /// window end.
    #[test]
    fn a_clip_without_a_curve_is_handed_back_to_rest() {
        let mut clip = clip_with(vec![]);

        clip.land_take(&[bend(100, 0x60), bend(300, 0x40), modw(400, 70)], BAR);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (100, vec![0xE0, 0x00, 0x60]),
                (300, vec![0xE0, 0x00, 0x40]),
                (400, vec![0xB0, 0x01, 70]),
                (BAR, vec![0xB0, 0x01, 0]),
            ]
        );
    }

    /// A span ending at the window's end writes nothing back: nothing plays
    /// there, and the wrap chases the clip's start.
    #[test]
    fn no_write_back_at_the_window_end() {
        let mut clip = clip_with(vec![modw(0, 30)]);

        clip.land_take(&[modw(BAR * 3, 90)], BAR * 4);

        assert_eq!(
            wheel_moves(&clip),
            vec![(0, vec![0xB0, 0x01, 30]), (BAR * 3, vec![0xB0, 0x01, 90])]
        );
    }

    /// Moves at or past the wheel end stay out, and with them their span.
    #[test]
    fn has_wheel_moves_ignores_notes() {
        let mut clip = clip_with(vec![Event::new(0, 0, vec![0x90, 60, 100])]);
        assert!(!clip.has_wheel_moves());

        clip.add_event(bend(100, 0x50));
        assert!(clip.has_wheel_moves());
    }

    #[test]
    fn moves_past_the_wheel_end_are_dropped() {
        let mut clip = clip_with(vec![modw(0, 30)]);

        clip.land_take(&[modw(BAR, 90), modw(BAR + 10, 100)], BAR);

        assert_eq!(wheel_moves(&clip), vec![(0, vec![0xB0, 0x01, 30])]);
    }

    /// The write-back keeps the take's channel.
    #[test]
    fn the_write_back_keeps_the_takes_channel() {
        let mut clip = clip_with(vec![]);

        clip.land_take(&[Event::new(100, 0, vec![0xE3, 0x00, 0x60])], BAR);

        assert_eq!(
            wheel_moves(&clip),
            vec![(100, vec![0xE3, 0x00, 0x60]), (BAR, vec![0xE3, 0x00, 0x40])]
        );
    }

    /// The values in force where a take starts are carried onto its start,
    /// a wheel at rest or already moving there is not.
    #[test]
    fn a_wheel_off_its_rest_is_carried_onto_the_start() {
        let mut clip = clip_with(vec![
            Event::new(100, 0, vec![0xE2, 0x00, 0x60]),
            modw(200, 40),
            modw(300, 0),
        ]);

        clip.carry_wheels_onto(BAR);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (100, vec![0xE2, 0x00, 0x60]),
                (200, vec![0xB0, 0x01, 40]),
                (300, vec![0xB0, 0x01, 0]),
                (BAR, vec![0xE2, 0x00, 0x60]),
            ]
        );

        let mut moving = clip_with(vec![modw(100, 40), modw(BAR, 60)]);
        moving.carry_wheels_onto(BAR);
        assert_eq!(
            wheel_moves(&moving),
            vec![(100, vec![0xB0, 0x01, 40]), (BAR, vec![0xB0, 0x01, 60])]
        );
    }
}
