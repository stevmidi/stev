//! Landing a captured take's controller moves in a clip — pitch bend, the
//! mod wheel and the knobs' CCs — by the latch rule of `370-wheel-capture.md`
//! § Decisions 5–7, which `390-knob-panel.md` decision 9 applies per
//! controller.
//!
//! A take's notes are added to the clip. Its controllers *replace*: for each
//! controller the take moved, the clip's own data for it goes from the take's
//! first move to the end of the take's window, the take's moves come in, and
//! at the window's end the value the clip's old curve had there is written
//! back, if it differs from the take's last. A controller the take didn't
//! move is left alone. A wheel off its rest where a take starts counts as a
//! move there ([`carry_wheels_onto`](Clip::carry_wheels_onto)); a knob's CC
//! has no rest, so it never does. Instead, where a clip has no earlier data
//! for a knob's CC, the value the knob stood at when the take turned it —
//! sent just ahead of the turn's first move (`knob_panel.rs`) — is written at
//! the window's start, so a loop wrap chases it as it chases a wheel's rest
//! ([`hold_cc_start_values`](Clip::hold_cc_start_values),
//! `390-knob-panel.md` decision 9).
//!
//! A lane's `Delete` is built on the same rule: a span of a wheel returned
//! to rest, a span of a knob's CC held at the value before it
//! ([`clear_controller_span`](Clip::clear_controller_span),
//! `380-wheel-lane.md` decision 18, `390-knob-panel.md` decision 13).

use std::ops::Range;

use crate::{
    core::midi::message::{Controller, parse_controller, rewrite_channel},
    models::{controllers::ControllerValues, event::Event},
};

use super::Clip;

impl Clip {
    /// The controller values in force just before event tick `tick`: per
    /// controller, the last unmuted move before it — a wheel's rest, or
    /// nothing for another CC, when there is none.
    pub(crate) fn controllers_before(&self, tick: i32) -> ControllerValues {
        let mut values = ControllerValues::default();
        for event in self.events.iter().take_while(|event| event.tick() < tick) {
            values.apply(event);
        }
        values
    }

    /// What a take starting at event tick `tick` carries: for each wheel off
    /// its rest there that doesn't move exactly there already, its last
    /// unmuted move before `tick` (value and channel), restamped at `tick`.
    /// Only the wheels: a CC with no rest is never "off" it.
    pub(crate) fn carried_wheel_moves(&self, tick: i32) -> Vec<Event> {
        Controller::WHEELS
            .into_iter()
            .filter_map(|wheel| {
                let (event, value) = self.last_move_before(wheel, tick)?;
                (Some(value) != wheel.rest() && !self.has_move_at(wheel, tick))
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

    /// For each CC with no rest (a knob's) that the clip moves after event
    /// tick `tick` but not at or before it: its first move after `tick` is
    /// copied to `tick` — where the knob stood when the turn began — so the
    /// stretch before the turn plays that value, not whatever the end of the
    /// last pass left. Then the turn's start value, sent on the same tick as
    /// its first move, goes from there
    /// ([`drop_superseded_controller_moves`](Self::drop_superseded_controller_moves)).
    /// A new clip's counterpart of [`carry_wheels_onto`](Self::carry_wheels_onto),
    /// called with the start of what the clip keeps.
    pub(crate) fn hold_cc_start_values(&mut self, tick: i32) {
        self.hold_start_values_of(tick, |_| true);
    }

    /// [`hold_cc_start_values`](Self::hold_cc_start_values) for the CCs
    /// `holds` accepts only — a landing's, the ones its take moved into a
    /// stretch the clip had no data for.
    fn hold_start_values_of(&mut self, tick: i32, holds: impl Fn(Controller) -> bool) {
        let mut seen: Vec<Controller> = Vec::new();
        let mut starts = Vec::new();
        for event in self.events.iter().filter(|event| !event.is_muted()) {
            let Some((controller, _)) = parse_controller(event.midi_message()) else {
                continue;
            };
            if controller.is_wheel() || !holds(controller) || seen.contains(&controller) {
                continue;
            }
            seen.push(controller);
            if event.tick() > tick {
                starts.push(Event::from_midi_with_tick(event.midi_message(), tick));
            }
        }
        if !starts.is_empty() {
            for event in starts {
                self.add_event(event);
            }
            self.sort_events_by_tick();
        }
        self.drop_superseded_controller_moves();
    }

    /// Drops every controller move another move of the same controller
    /// follows on the same tick: only the last sounds. A turn's start value
    /// goes out just ahead of its first move, on the same tick; once held
    /// from the window's start, or in force there already, the move itself
    /// is all that's left to keep there.
    fn drop_superseded_controller_moves(&mut self) {
        let superseded: Vec<bool> = self
            .events
            .iter()
            .enumerate()
            .map(|(i, event)| {
                let Some((controller, _)) = parse_controller(event.midi_message()) else {
                    return false;
                };
                self.events[i + 1..]
                    .iter()
                    .take_while(|later| later.tick() == event.tick())
                    .any(|later| is_move_of(later, controller))
            })
            .collect();
        if !superseded.contains(&true) {
            return;
        }
        let mut flags = superseded.into_iter();
        self.events.retain(|_| !flags.next().unwrap_or(false));
        self.update_delta_ticks();
    }

    /// Lands `take` — a captured take in this clip's event ticks — by the
    /// latch rule (see the module docs). Its notes are added. For each
    /// controller it moves before `wheel_end` (never past the window's end),
    /// the clip's own moves of that controller from the take's first move up
    /// to `wheel_end` make way for the take's, and the old curve's value at
    /// `wheel_end` is written back there when the clip has one, it differs
    /// from the take's last and the clip doesn't move that controller exactly
    /// there anyway — unless `wheel_end` is the window's end, where the clip
    /// loops and the chase at the wrap hands back. A CC with no rest the take
    /// moves, which the clip has no move of before the take's first, is then
    /// held from the window's start at the value the turn started from
    /// ([`hold_cc_start_values`](Self::hold_cc_start_values)).
    /// Take moves at or past `wheel_end` are dropped. Re-sorted and note
    /// lengths recomputed.
    pub(crate) fn land_take(&mut self, take: &[Event], wheel_end: i32) {
        let (window_start, window_end) = (self.region().start(), self.region().end());
        let wheel_end = wheel_end.min(window_end);

        let mut controllers: Vec<Controller> = take
            .iter()
            .filter_map(|event| parse_controller(event.midi_message()))
            .map(|(controller, _)| controller)
            .collect();
        controllers.sort_by_key(|controller| controller.index());
        controllers.dedup();
        // The CCs the clip has no data for before the take moves them: their
        // stretch before the turn is held at its start value.
        let mut unheld = Vec::new();

        for controller in controllers {
            let moves: Vec<(&Event, u16)> = take
                .iter()
                .filter(|event| event.tick() < wheel_end)
                .filter_map(|event| Some((event, move_value(event, controller)?)))
                .collect();
            let (Some(first), Some(&(last, last_value))) = (
                moves.iter().map(|(event, _)| event.tick()).min(),
                moves.iter().max_by_key(|(event, _)| event.tick()),
            ) else {
                continue;
            };
            if self.last_move_before(controller, first).is_none() {
                unheld.push(controller);
            }
            let channel = last.midi_channel().unwrap_or(0);
            self.replace_controller_span(
                controller,
                first..wheel_end,
                Some(last_value),
                Some(channel),
                window_end,
            );
        }

        for event in take {
            let is_late_move = !event.is_note_edge() && event.tick() >= wheel_end;
            if !is_late_move {
                self.add_event(event.clone());
            }
        }
        self.sort_events_by_tick();
        self.hold_start_values_of(window_start, |controller| unheld.contains(&controller));
        self.calculate_note_lengths();
    }

    /// Clears `controller` over event ticks `start..end` — a lane's `Delete`
    /// (`380-wheel-lane.md` decision 18, `390-knob-panel.md` decision 13).
    /// Every move of `controller` inside the span goes, muted ones too. A
    /// wheel is returned to rest there, the same result as riding it at rest
    /// across the span under the landing rule: a rest move goes in at `start`
    /// unless the wheel is at rest there already. A knob's CC has no rest, so
    /// the value in force just before the span holds across it instead. Either
    /// way the value in force at `end` is written back there, so the curve
    /// after the span is unchanged — unless it is what the span holds, the
    /// clip moves the controller exactly at `end` anyway, or nothing follows
    /// (`end` at or past [`event_extent`](Self::event_extent)'s end). The
    /// other controllers and the notes are left alone. Returns whether
    /// anything changed.
    pub(crate) fn clear_controller_span(
        &mut self,
        controller: Controller,
        start: i32,
        end: i32,
    ) -> bool {
        if start >= end {
            return false;
        }
        // What the span holds: a wheel's rest, a CC's value before it. Set
        // at the start when the value in force there differs — never for a
        // CC, which holds that very value.
        let before = self
            .last_move_before(controller, start)
            .map(|(event, value)| (event.midi_channel().unwrap_or(0), value));
        let held = controller.rest().or(before.map(|(_, value)| value));
        let set_at_start = before.zip(held).filter(|&((_, value), held)| value != held);
        let extent_end = self.event_extent().1;
        let mut changed =
            self.replace_controller_span(controller, start..end, held, None, extent_end);
        if let Some(((channel, _), held)) = set_at_start {
            self.add_controller_move(controller, held, channel, start);
            changed = true;
        }
        if changed {
            self.sort_events_by_tick();
        }
        changed
    }

    /// The landing rule's step for one controller, shared by a take and a
    /// rest: the clip's moves of `controller` in `span` go, muted ones too,
    /// and the value the old curve had at the span's end — a wheel's rest
    /// where nothing moved it, nothing for a CC nothing moved — is written
    /// back there when it differs from `span_last` (what the span ends on,
    /// `None` for a CC the span leaves with no value in force)
    /// and the clip doesn't move the controller exactly there — unless the
    /// end is at or past `hand_back_before`, where nothing follows to hand
    /// back to. The write-back goes on `channel`, or the old value's own.
    /// Leaves the events unsorted; returns whether anything changed.
    fn replace_controller_span(
        &mut self,
        controller: Controller,
        span: Range<i32>,
        span_last: Option<u16>,
        channel: Option<u8>,
        hand_back_before: i32,
    ) -> bool {
        let end = span.end;
        let old = self.last_move_before(controller, end);
        let old_value = old.map(|(_, value)| value).or(controller.rest());
        let channel = channel
            .or_else(|| old.and_then(|(event, _)| event.midi_channel()))
            .unwrap_or(0);
        let write_back = end < hand_back_before
            && old_value.is_some_and(|value| Some(value) != span_last)
            && !self.has_move_at(controller, end);

        let len = self.events.len();
        self.events
            .retain(|event| !(is_move_of(event, controller) && span.contains(&event.tick())));
        let removed = self.events.len() != len;
        if let Some(old_value) = old_value.filter(|_| write_back) {
            self.add_controller_move(controller, old_value, channel, end);
        }
        removed || write_back
    }

    /// Adds a move of `controller` to `value` on `channel` at event tick
    /// `tick`, unsorted.
    fn add_controller_move(&mut self, controller: Controller, value: u16, channel: u8, tick: i32) {
        let mut message = controller.message(value);
        rewrite_channel(&mut message, channel);
        self.add_event(Event::from_midi_with_tick(&message, tick));
    }

    /// The clip's whole extent in event ticks, `(start, end)`: from tick 0,
    /// or an earlier event, to the window's end or one past the last event,
    /// whichever is later — material outside the window included. The span
    /// `⌘A` marks in a wheel lane (`380-wheel-lane.md` decision 17). Not
    /// [`reach`](Self::reach), which frames the notes with headroom: this
    /// covers every event, wheel moves included, and nothing more. The
    /// events are kept in tick order, so the ends are the first and last.
    pub(crate) fn event_extent(&self) -> (i32, i32) {
        let (first, last) = (self.events.first(), self.events.last());
        let start = first.map_or(0, Event::tick).min(0);
        let end = last
            .map_or(i32::MIN, |event| event.tick() + 1)
            .max(self.region().end());
        (start, end)
    }

    /// `controller`'s last unmuted move before event tick `tick` and the
    /// value it moved to — the value in force there, with its channel.
    fn last_move_before(&self, controller: Controller, tick: i32) -> Option<(&Event, u16)> {
        self.events
            .iter()
            .take_while(|event| event.tick() < tick)
            .filter(|event| !event.is_muted())
            .filter_map(|event| Some((event, move_value(event, controller)?)))
            .last()
    }

    /// Whether the clip holds a controller move — a wheel's or a knob's.
    pub(crate) fn has_controller_moves(&self) -> bool {
        self.events
            .iter()
            .any(|event| parse_controller(event.midi_message()).is_some())
    }

    /// Whether the clip has a move of `controller` exactly at event tick
    /// `tick`.
    fn has_move_at(&self, controller: Controller, tick: i32) -> bool {
        self.events
            .iter()
            .any(|event| event.tick() == tick && is_move_of(event, controller))
    }
}

/// The value `event` moves `controller` to, muted or not; `None` when it
/// moves something else.
fn move_value(event: &Event, controller: Controller) -> Option<u16> {
    parse_controller(event.midi_message())
        .and_then(|(moved, value)| (moved == controller).then_some(value))
}

/// Whether `event` moves `controller`, muted or not.
fn is_move_of(event: &Event, controller: Controller) -> bool {
    move_value(event, controller).is_some()
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

    /// Notes aren't controller moves; a wheel's or a knob's are.
    #[test]
    fn has_controller_moves_ignores_notes() {
        let mut clip = clip_with(vec![Event::new(0, 0, vec![0x90, 60, 100])]);
        assert!(!clip.has_controller_moves());

        clip.add_event(bend(100, 0x50));
        assert!(clip.has_controller_moves());
        let mut knobbed = clip_with(vec![knob(100, 14, 5)]);
        assert!(knobbed.has_controller_moves());
        knobbed.add_event(Event::new(0, 0, vec![0x90, 60, 100]));
        assert!(knobbed.has_controller_moves());
    }

    /// Moves at or past the wheel end stay out, and with them their span.
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

    /// The moves inside the span go, muted ones too; the wheel is set to
    /// rest at the start and the value in force at the end is written back.
    #[test]
    fn clearing_a_wheel_span_clears_the_span_and_hands_back() {
        let mut clip = swept();
        let mut muted = modw(BAR + 960, 99);
        muted.set_muted(true);
        clip.add_event(muted);
        clip.sort_events_by_tick();

        assert!(clip.clear_controller_span(Controller::MOD_WHEEL, BAR + 480, BAR * 2 + 480));

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (BAR, vec![0xB0, 0x01, 40]),
                (BAR + 480, vec![0xB0, 0x01, 0]),
                (BAR * 2 + 480, vec![0xB0, 0x01, 60]),
                (BAR * 3, vec![0xB0, 0x01, 80]),
            ]
        );
    }

    /// A wheel already at rest at the span's start gets no rest move there,
    /// and a rest value at its end needs no write-back.
    #[test]
    fn clearing_a_wheel_span_adds_nothing_where_the_wheel_rests() {
        let mut clip = clip_with(vec![bend(BAR, 0x60), bend(BAR + 500, 0x40)]);

        assert!(clip.clear_controller_span(Controller::PitchBend, 0, BAR * 2));

        assert_eq!(wheel_moves(&clip), vec![]);
    }

    /// A move exactly at the span's end already hands back; nothing outside
    /// the span changes.
    #[test]
    fn clearing_a_wheel_span_keeps_a_move_at_the_end() {
        let mut clip = swept();

        assert!(clip.clear_controller_span(Controller::MOD_WHEEL, BAR, BAR * 2));

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 0x01, 20]),
                (BAR, vec![0xB0, 0x01, 0]),
                (BAR * 2, vec![0xB0, 0x01, 60]),
                (BAR * 3, vec![0xB0, 0x01, 80]),
            ]
        );
    }

    /// The whole extent (`⌘A`) leaves no moves of the wheel, material past
    /// the window included, and the other wheel and the notes untouched.
    #[test]
    fn clearing_a_wheel_span_over_the_whole_clip_clears_the_wheel() {
        let mut clip = clip_with(vec![
            modw(0, 30),
            bend(100, 0x60),
            Event::new(200, 0, vec![0x90, 60, 100]),
            Event::new(400, 0, vec![0x80, 60, 0]),
            modw(BAR * 5, 90),
        ]);
        let (start, end) = clip.event_extent();
        assert_eq!((start, end), (0, BAR * 5 + 1));

        assert!(clip.clear_controller_span(Controller::MOD_WHEEL, start, end));

        assert_eq!(wheel_moves(&clip), vec![(100, vec![0xE0, 0x00, 0x60])]);
        assert_eq!(clip.events().iter().filter(|e| e.is_note_edge()).count(), 2);
    }

    /// The rest move and the write-back keep the wheel's channel.
    #[test]
    fn clearing_a_wheel_span_keeps_the_channel() {
        let mut clip = clip_with(vec![Event::new(100, 0, vec![0xE3, 0x00, 0x60])]);

        assert!(clip.clear_controller_span(Controller::PitchBend, 200, 300));

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (100, vec![0xE3, 0x00, 0x60]),
                (200, vec![0xE3, 0x00, 0x40]),
                (300, vec![0xE3, 0x00, 0x60]),
            ]
        );
    }

    /// A span over a wheel at rest throughout changes nothing.
    #[test]
    fn clearing_a_wheel_span_at_rest_is_a_no_op() {
        let mut clip = clip_with(vec![modw(BAR * 2, 50), bend(BAR, 0x60)]);
        let before = wheel_moves(&clip);

        assert!(!clip.clear_controller_span(Controller::MOD_WHEEL, 0, BAR));
        assert!(!clip.clear_controller_span(Controller::MOD_WHEEL, BAR, BAR));

        assert_eq!(wheel_moves(&clip), before);
    }

    fn knob(tick: i32, cc: u8, value: u8) -> Event {
        Event::new(tick, 0, vec![0xB0, cc, value])
    }

    /// A knob's CC has no rest: where the clip had no data for it, nothing
    /// is written back at the window's end — the CC stays where the take
    /// left it, as a hardware knob would. The value the turn started from
    /// (sent ahead of its first move, on its tick) goes to the window's
    /// start instead, so a wrap chases it there, and its copy on the turn's
    /// tick, superseded, goes.
    #[test]
    fn a_knob_take_on_a_clip_without_its_cc_holds_its_start_value_from_the_window_start() {
        let mut clip = clip_with(vec![]);

        clip.land_take(
            &[knob(100, 14, 20), knob(100, 14, 30), knob(300, 14, 90)],
            BAR,
        );

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 14, 20]),
                (100, vec![0xB0, 14, 30]),
                (300, vec![0xB0, 14, 90])
            ]
        );
    }

    /// A turn starting on the window's start needs nothing held before it.
    #[test]
    fn a_knob_take_from_the_window_start_holds_nothing_more() {
        let mut clip = clip_with(vec![]);

        clip.land_take(&[knob(0, 14, 20), knob(0, 14, 30)], BAR);

        assert_eq!(wheel_moves(&clip), vec![(0, vec![0xB0, 14, 30])]);
    }

    /// A new clip holds each knob's first value from where it keeps
    /// material — not a wheel's (it has a rest), nor a knob's the clip
    /// already moves by then.
    #[test]
    fn a_new_clip_holds_each_knobs_start_value_from_its_start() {
        let mut clip = clip_with(vec![
            knob(-100, 15, 40),
            modw(200, 50),
            knob(300, 14, 20),
            knob(300, 14, 30),
            knob(500, 15, 60),
        ]);

        clip.hold_cc_start_values(0);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (-100, vec![0xB0, 15, 40]),
                (0, vec![0xB0, 14, 20]),
                (200, vec![0xB0, 0x01, 50]),
                (300, vec![0xB0, 14, 30]),
                (500, vec![0xB0, 15, 60]),
            ]
        );
    }

    /// Where the clip has a curve for the knob's CC, the take replaces it
    /// from its first move and the old value comes back at the span's end.
    #[test]
    fn a_knob_take_latches_and_hands_back_like_a_wheel() {
        let mut clip = clip_with(vec![knob(0, 14, 10), knob(BAR * 2, 14, 50)]);

        clip.land_take(&[knob(BAR, 14, 100), knob(BAR + 200, 14, 120)], BAR * 3);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 14, 10]),
                (BAR, vec![0xB0, 14, 100]),
                (BAR + 200, vec![0xB0, 14, 120]),
                (BAR * 3, vec![0xB0, 14, 50]),
            ]
        );
    }

    /// A take turning one knob leaves the other knobs and the wheels alone,
    /// and a knob's CC is never carried into a take as a wheel is.
    #[test]
    fn a_knob_take_never_touches_the_other_knobs_or_the_wheels() {
        let mut clip = clip_with(vec![
            knob(100, 15, 40),
            modw(200, 50),
            knob(400, 14, 20),
            bend(500, 0x60),
            knob(BAR + 100, 15, 60),
        ]);
        assert!(clip.carried_wheel_moves(BAR).iter().all(|event| {
            parse_controller(event.midi_message()).is_some_and(|(c, _)| c.is_wheel())
        }));

        clip.land_take(&[knob(600, 14, 90)], BAR * 2);

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (100, vec![0xB0, 15, 40]),
                (200, vec![0xB0, 0x01, 50]),
                (400, vec![0xB0, 14, 20]),
                (500, vec![0xE0, 0x00, 0x60]),
                (600, vec![0xB0, 14, 90]),
                (BAR + 100, vec![0xB0, 15, 60]),
                (BAR * 2, vec![0xB0, 14, 20]),
            ]
        );
    }

    /// A knob's CC has no rest: the moves inside the span go, the value in
    /// force before it holds across it (no move at the start), and the value
    /// in force at the span's end is written back there.
    #[test]
    fn clearing_a_cc_span_holds_the_value_before_it() {
        let mut clip = clip_with(vec![
            knob(0, 14, 30),
            knob(BAR, 14, 70),
            knob(BAR + 480, 14, 90),
            knob(BAR * 3, 14, 10),
            modw(BAR + 100, 50),
        ]);

        assert!(clip.clear_controller_span(Controller::Cc(14), BAR - 10, BAR * 2));

        assert_eq!(
            wheel_moves(&clip),
            vec![
                (0, vec![0xB0, 14, 30]),
                (BAR + 100, vec![0xB0, 0x01, 50]),
                (BAR * 2, vec![0xB0, 14, 90]),
                (BAR * 3, vec![0xB0, 14, 10]),
            ]
        );
    }

    /// A CC with no value before the span is left with none there: nothing
    /// is written at the start, only the value in force at the end is handed
    /// back.
    #[test]
    fn clearing_a_cc_span_with_nothing_before_writes_only_the_end() {
        let mut clip = clip_with(vec![knob(100, 14, 30), knob(BAR * 2, 14, 60)]);

        assert!(clip.clear_controller_span(Controller::Cc(14), 0, BAR));

        assert_eq!(
            wheel_moves(&clip),
            vec![(BAR, vec![0xB0, 14, 30]), (BAR * 2, vec![0xB0, 14, 60])]
        );
    }

    /// `⌘A` then `Delete` clears one CC from the clip and nothing else: the
    /// other knobs' CCs, the wheels and the notes stay.
    #[test]
    fn clearing_a_cc_over_the_whole_clip_clears_that_cc_only() {
        let mut clip = clip_with(vec![
            knob(0, 14, 30),
            knob(50, 15, 40),
            modw(80, 20),
            Event::new(200, 0, vec![0x90, 60, 100]),
            Event::new(400, 0, vec![0x80, 60, 0]),
            knob(BAR * 4, 14, 90),
        ]);
        let (start, end) = clip.event_extent();

        assert!(clip.clear_controller_span(Controller::Cc(14), start, end));

        assert_eq!(
            wheel_moves(&clip),
            vec![(50, vec![0xB0, 15, 40]), (80, vec![0xB0, 0x01, 20])]
        );
        assert_eq!(clip.events().iter().filter(|e| e.is_note_edge()).count(), 2);
    }

    /// A span over a CC that holds one value throughout changes nothing.
    #[test]
    fn clearing_a_cc_span_with_no_moves_in_it_is_a_no_op() {
        let mut clip = clip_with(vec![knob(100, 14, 30), knob(BAR * 3, 14, 60)]);
        let before = wheel_moves(&clip);

        assert!(!clip.clear_controller_span(Controller::Cc(14), BAR, BAR * 2));

        assert_eq!(wheel_moves(&clip), before);
    }
}
