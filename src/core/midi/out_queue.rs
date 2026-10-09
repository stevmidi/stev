//! Deadline-ordered holding queue for outbound MIDI.
//!
//! Clip events reach the `"midiout"` thread carrying the [`Instant`] their tick
//! was intended to sound at. The thread parks them here until that instant plus
//! the user's MIDI-output offset, so notes bound for external gear leave in step
//! with the plugin and click paths instead of running ahead of them — those two
//! are scheduled a whole audio buffer into the future
//! ([`SCHEDULE_DELAY_FRAMES`](crate::core::audio::SCHEDULE_DELAY_FRAMES)) while
//! an undelayed port write goes out immediately. See `160-midi-out-offset.md`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One outbound MIDI message on its way to the `"midiout"` thread.
pub(crate) struct MidiOutMessage {
    /// The raw MIDI bytes.
    pub(crate) bytes: Vec<u8>,
    /// The tick [`Instant`] this message belongs to, or `None` to send as soon
    /// as the output thread picks it up. Only clip playback carries an instant;
    /// live thru, note previews, chased notes and the note-off safety net are
    /// all immediate — see the module docs.
    pub(crate) at: Option<Instant>,
}

impl MidiOutMessage {
    /// A message to send the moment the output thread picks it up.
    pub(crate) fn now(bytes: Vec<u8>) -> Self {
        MidiOutMessage { bytes, at: None }
    }

    /// A clip message belonging to the tick intended to sound at `at`.
    pub(crate) fn at(bytes: Vec<u8>, at: Instant) -> Self {
        MidiOutMessage {
            bytes,
            at: Some(at),
        }
    }
}

/// The instant `at` should reach the output port, or `None` to send it right
/// away. A message with no tick instant, or one whose deadline has already
/// passed by the time the thread sees it, is immediate — so the queue only ever
/// holds messages with a real wait still ahead of them, and an offset of zero
/// reproduces the undelayed behaviour exactly.
pub(crate) fn deadline_for(at: Option<Instant>, offset_ms: i32, now: Instant) -> Option<Instant> {
    let at = at?;
    let offset = Duration::from_millis(offset_ms.max(0) as u64);
    let deadline = at.checked_add(offset)?;
    (deadline > now).then_some(deadline)
}

/// Pending messages, earliest deadline first.
#[derive(Default)]
pub(crate) struct MidiOutQueue {
    /// `(deadline, bytes)` pairs, earliest deadline first.
    pending: VecDeque<(Instant, Vec<u8>)>,
}

impl MidiOutQueue {
    /// An empty queue.
    pub(crate) fn new() -> Self {
        MidiOutQueue::default()
    }

    /// Inserts `bytes` at `deadline`, after any message already queued for the
    /// same instant. Deadlines normally arrive in order, so the scan starts from
    /// the newest entry; holding arrival order among equal deadlines is what
    /// stops a note-off overtaking the note-on it belongs to.
    pub(crate) fn push(&mut self, deadline: Instant, bytes: Vec<u8>) {
        let index = self
            .pending
            .iter()
            .rposition(|(queued, _)| *queued <= deadline)
            .map_or(0, |i| i + 1);
        self.pending.insert(index, (deadline, bytes));
    }

    /// Removes and returns the earliest message whose deadline has arrived.
    pub(crate) fn pop_due(&mut self, now: Instant) -> Option<Vec<u8>> {
        match self.pending.front() {
            Some((deadline, _)) if *deadline <= now => {
                self.pending.pop_front().map(|(_, bytes)| bytes)
            }
            _ => None,
        }
    }

    /// When the next queued message comes due, if anything is waiting.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending.front().map(|(deadline, _)| *deadline)
    }

    /// Drops every pending message. Used when the transport stops or the port
    /// is swapped: scheduled note-ons belong to a playback that has ended, and
    /// `NoteLogger` only ever logged the notes that actually reached the port,
    /// so its release pass still covers everything left sounding.
    pub(crate) fn clear(&mut self) {
        self.pending.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_on(note: u8) -> Vec<u8> {
        vec![0x90, note, 100]
    }

    #[test]
    fn pops_in_deadline_order_regardless_of_push_order() {
        let origin = Instant::now();
        let mut queue = MidiOutQueue::new();
        queue.push(origin + Duration::from_millis(30), note_on(62));
        queue.push(origin + Duration::from_millis(10), note_on(60));
        queue.push(origin + Duration::from_millis(20), note_on(61));

        let late = origin + Duration::from_millis(100);
        assert_eq!(queue.pop_due(late), Some(note_on(60)));
        assert_eq!(queue.pop_due(late), Some(note_on(61)));
        assert_eq!(queue.pop_due(late), Some(note_on(62)));
        assert_eq!(queue.pop_due(late), None);
    }

    #[test]
    fn equal_deadlines_keep_arrival_order() {
        // A note-off and the note-on that follows it land on the same tick when
        // a clip repeats a pitch back to back. Reordering them leaves the note
        // hanging, so insertion must be stable.
        let origin = Instant::now();
        let deadline = origin + Duration::from_millis(5);
        let mut queue = MidiOutQueue::new();
        queue.push(deadline, vec![0x80, 60, 0]);
        queue.push(deadline, note_on(60));

        let late = origin + Duration::from_millis(100);
        assert_eq!(queue.pop_due(late), Some(vec![0x80, 60, 0]));
        assert_eq!(queue.pop_due(late), Some(note_on(60)));
    }

    #[test]
    fn nothing_pops_before_its_deadline() {
        let origin = Instant::now();
        let mut queue = MidiOutQueue::new();
        queue.push(origin + Duration::from_millis(10), note_on(60));

        assert_eq!(queue.pop_due(origin + Duration::from_millis(9)), None);
        assert_eq!(
            queue.next_deadline(),
            Some(origin + Duration::from_millis(10))
        );
        assert_eq!(
            queue.pop_due(origin + Duration::from_millis(10)),
            Some(note_on(60))
        );
        assert_eq!(queue.next_deadline(), None);
    }

    #[test]
    fn clear_drops_everything_pending() {
        let origin = Instant::now();
        let mut queue = MidiOutQueue::new();
        queue.push(origin + Duration::from_millis(10), note_on(60));
        queue.push(origin + Duration::from_millis(20), note_on(62));
        assert_eq!(queue.len(), 2);

        queue.clear();

        assert_eq!(queue.len(), 0);
        assert_eq!(queue.next_deadline(), None);
        assert_eq!(queue.pop_due(origin + Duration::from_millis(100)), None);
    }

    #[test]
    fn a_message_with_no_instant_is_immediate() {
        let now = Instant::now();
        assert_eq!(deadline_for(None, 50, now), None);
    }

    #[test]
    fn an_offset_that_has_already_elapsed_is_immediate() {
        let now = Instant::now();
        let at = now - Duration::from_millis(20);
        assert_eq!(deadline_for(Some(at), 5, now), None);
    }

    #[test]
    fn a_pending_offset_yields_the_tick_instant_plus_the_offset() {
        let now = Instant::now();
        let at = now - Duration::from_millis(2);
        assert_eq!(
            deadline_for(Some(at), 20, now),
            Some(at + Duration::from_millis(20))
        );
    }

    #[test]
    fn a_zero_or_negative_offset_is_immediate_for_a_tick_already_past() {
        // The tick instant is always slightly behind by the time the output
        // thread sees it, so offset 0 reproduces the undelayed send exactly.
        let now = Instant::now();
        let at = now - Duration::from_micros(500);
        assert_eq!(deadline_for(Some(at), 0, now), None);
        assert_eq!(deadline_for(Some(at), -10, now), None);
    }
}
