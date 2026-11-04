//! A CLAP plugin's parameters, as the knobs reach them
//! (`400-parameter-knobs.md`): the mapping between CLAP's plain values and
//! Stev's normalized ones, and the two rings between the editor and the voice.
//!
//! Everything goes through `process`. A knob turned in Stev crosses to the
//! voice as a move ([`EditorParams::send`]) and goes in as a parameter value
//! event, and a clip's move is queued there directly. What the plugin reports
//! comes out of `process` as output events: [`ReportFeed`] takes them straight
//! off the plugin's `try_push`, with nothing allocated, and they cross back to
//! be read as [`ParameterGesture`]s ([`ParamValues::gesture`]).
//!
//! CLAP's `get_value` is main-thread only and needs a `&mut` handle, and a
//! turn's start has to be known once the turn has already moved the value, so
//! the editor keeps every parameter's value here instead of asking the
//! plugin: [`ParamValues`] follows the reports, Stev's own moves and a rescan.

use std::collections::HashMap;
use std::sync::Arc;

use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{EventBuffer, OutputEventBuffer, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::events::{EventFlags, Pckn, UnknownEvent};
use clack_host::utils::ClapId;
use log::debug;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::core::plugin_host::editor::{ParameterGesture, ParameterInfo};
use crate::core::plugin_host::ui_wake::UiWake;
use crate::core::plugin_host::voice::EVENT_CAPACITY;
use crate::models::knobs::ParamId;

/// Capacity of each ring. A plugin loading a preset can report a parameter per
/// control at once, so this is sized for a whole panel's worth.
const PARAM_RING_CAPACITY: usize = 4096;

/// How far apart two normalized values must be to count as a move: a plain
/// value mapped there and back lands a rounding error off.
const SAME_VALUE: f64 = 1e-9;

/// A parameter's plain range, fixed for as long as the plugin stays active:
/// CLAP only lets it change while deactivated, and Stev never deactivates a
/// loaded plugin (`request_restart` is ignored).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ParamRange {
    /// Its lowest plain value.
    min: f64,
    /// Its highest plain value.
    max: f64,
    /// Whether it takes whole numbers only (`CLAP_PARAM_IS_STEPPED`).
    stepped: bool,
}

impl ParamRange {
    /// `plain` as a share of the range, `0.0..=1.0`. A range with nothing in
    /// it reads as 0.
    fn normalized(self, plain: f64) -> f64 {
        let span = self.max - self.min;
        if span > 0.0 {
            ((plain - self.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The plain value `normalized` of the way up the range. A stepped value
    /// is rounded, since the plugin truncates: a position a rounding error
    /// under its whole number would otherwise land a step low.
    fn plain(self, normalized: f64) -> f64 {
        let plain = self.min + normalized.clamp(0.0, 1.0) * (self.max - self.min);
        if self.stepped { plain.round() } else { plain }
    }

    /// How many steps it moves in, 0 for a parameter that moves smoothly.
    fn steps(self) -> u32 {
        if self.stepped {
            (self.max - self.min).round() as u32
        } else {
            0
        }
    }
}

/// One parameter, as read from the plugin at load.
pub(super) struct ClapParam {
    /// What the knob panel shows of it.
    info: ParameterInfo,
    /// Its plain range.
    range: ParamRange,
}

impl ClapParam {
    /// A parameter called `name`, ranging over `min..=max`, whose default is
    /// the plain value `default`.
    pub(super) fn new(name: String, min: f64, max: f64, default: f64, stepped: bool) -> Self {
        let range = ParamRange { min, max, stepped };
        Self {
            info: ParameterInfo {
                name,
                default_value: range.normalized(default),
                steps: range.steps(),
            },
            range,
        }
    }
}

/// Every parameter of a plugin, by ID. Read once at load, as VST3's are: a
/// plugin that renames its parameters later keeps the old names until it is
/// loaded again. Shared with the audio thread, which only reads it.
type ParamTable = HashMap<ParamId, ClapParam>;

/// What the plugin said in `process` about a parameter, crossing to the main
/// thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ParamReport {
    /// A turn began (a gesture-begin event), at this monotonic time.
    Begin(ParamId, u64),
    /// The parameter moved (a value event).
    Value {
        /// Which parameter.
        id: ParamId,
        /// Where it went, plain.
        plain: f64,
        /// When, on the monotonic timeline.
        at_ns: u64,
        /// False when the plugin says not to record it
        /// (`CLAP_EVENT_DONT_RECORD`): its own MIDI mapping moved it, not the
        /// user's hand on its editor.
        recorded: bool,
    },
    /// The turn ended (a gesture-end event).
    End(ParamId),
}

/// `event` as a [`ParamReport`], if it is about a parameter. `at_ns` gives the
/// monotonic time of a sample offset in the block.
fn param_report(event: &UnknownEvent, at_ns: impl Fn(u32) -> u64) -> Option<ParamReport> {
    let time = event.header().time();
    match event.as_core_event()? {
        CoreEventSpace::ParamGestureBegin(begin) => {
            Some(ParamReport::Begin(begin.param_id()?.get(), at_ns(time)))
        }
        CoreEventSpace::ParamValue(value) => Some(ParamReport::Value {
            id: value.param_id()?.get(),
            plain: value.value(),
            at_ns: at_ns(time),
            recorded: !event.header().flags().contains(EventFlags::DONT_RECORD),
        }),
        CoreEventSpace::ParamGestureEnd(end) => Some(ParamReport::End(end.param_id()?.get())),
        _ => None,
    }
}

/// The monotonic time `time` samples into a block that began at `block_ns`.
fn event_nanos(block_ns: u64, time: u32, ns_per_frame: f64) -> u64 {
    block_ns + (f64::from(time) * ns_per_frame) as u64
}

/// Every parameter's value in force, normalized, kept from what the plugin
/// reports and what Stev sends: see the module docs.
#[derive(Default)]
pub(super) struct ParamValues {
    /// The plugin's parameters.
    table: Arc<ParamTable>,
    /// Where each stands.
    values: HashMap<ParamId, f64>,
}

impl ParamValues {
    /// The parameters of `params`, each with the plain value it stands at.
    pub(super) fn new(params: impl IntoIterator<Item = (ParamId, ClapParam, f64)>) -> Self {
        let mut table = ParamTable::new();
        let mut values = HashMap::new();
        for (id, param, plain) in params {
            values.insert(id, param.range.normalized(plain));
            table.insert(id, param);
        }
        Self {
            table: Arc::new(table),
            values,
        }
    }

    /// What `report` means to the knob panel, if anything, keeping the value.
    ///
    /// A turn's start carries the value from before the turn: reports are read
    /// in order, so it is still here. A value that changes nothing is no move:
    /// a plugin that repeats back what Stev sent it mustn't read as the user's
    /// turn. One the plugin says not to record moves the knob and nothing else.
    fn gesture(&mut self, report: ParamReport) -> Option<ParameterGesture> {
        match report {
            ParamReport::Begin(id, at_ns) => {
                let start = *self.values.get(&id)?;
                Some(ParameterGesture::Begin(id, Some(start), at_ns))
            }
            ParamReport::Value {
                id,
                plain,
                at_ns,
                recorded,
            } => {
                let value = self.table.get(&id)?.range.normalized(plain);
                let before = self.values.insert(id, value);
                let moved = before.is_none_or(|before| (before - value).abs() >= SAME_VALUE);
                (moved && recorded).then_some(ParameterGesture::Move(id, value, at_ns))
            }
            ParamReport::End(id) => self
                .values
                .contains_key(&id)
                .then_some(ParameterGesture::End(id)),
        }
    }

    /// Reads every value again after the plugin said they changed (a rescan:
    /// a preset it loaded), with `read` giving a parameter's plain value. Each
    /// one that moved is reported as a move outside any turn, as VST3's
    /// `performEdit` reports a preset.
    fn reread(
        &mut self,
        read: impl Fn(ParamId) -> Option<f64>,
        at_ns: u64,
    ) -> Vec<ParameterGesture> {
        let table = Arc::clone(&self.table);
        table
            .keys()
            .filter_map(|&id| {
                self.gesture(ParamReport::Value {
                    id,
                    plain: read(id)?,
                    at_ns,
                    recorded: true,
                })
            })
            .collect()
    }
}

/// The editor's end of the bridge to the voice.
pub(super) struct EditorParams {
    /// The values in force.
    values: ParamValues,
    /// Moves for the voice to send: an ID and a normalized value.
    moves: Producer<(ParamId, f64)>,
    /// What the plugin reported.
    reports: Consumer<ParamReport>,
}

impl EditorParams {
    /// Whether the plugin has any parameters.
    pub(super) fn is_empty(&self) -> bool {
        self.values.table.is_empty()
    }

    /// Parameter `id`'s name and default.
    pub(super) fn info(&self, id: ParamId) -> Option<&ParameterInfo> {
        self.values.table.get(&id).map(|param| &param.info)
    }

    /// Parameter `id`'s value in force, normalized.
    pub(super) fn value(&self, id: ParamId) -> Option<f64> {
        self.values.values.get(&id).copied()
    }

    /// Sends parameter `id` to `value` (normalized), the sound and the
    /// plugin's editor following from the event `process` gets. Dropped if the
    /// ring is full, which costs that one move rather than blocking the UI.
    pub(super) fn send(&mut self, id: ParamId, value: f64) {
        self.follow(id, value);
        if self.moves.push((id, value)).is_err() {
            debug!("clap: parameter ring full; dropped a move of {id}");
        }
    }

    /// Notes that playback put parameter `id` at `value`. The plugin heard it
    /// on its sample and keeps its own editor up to date, so this is all a
    /// played move needs.
    pub(super) fn follow(&mut self, id: ParamId, value: f64) {
        if self.values.table.contains_key(&id) {
            self.values.values.insert(id, value.clamp(0.0, 1.0));
        }
    }

    /// What the plugin reported since the last call, oldest first.
    pub(super) fn take_gestures(&mut self) -> Vec<ParameterGesture> {
        let mut gestures = Vec::new();
        while let Ok(report) = self.reports.pop() {
            gestures.extend(self.values.gesture(report));
        }
        gestures
    }

    /// See [`ParamValues::reread`].
    pub(super) fn reread(
        &mut self,
        read: impl Fn(ParamId) -> Option<f64>,
        at_ns: u64,
    ) -> Vec<ParameterGesture> {
        self.values.reread(read, at_ns)
    }
}

/// The plugin's output events for one block: the buffer `process` writes to.
/// Keeps only what is about a parameter, and owns no storage of its own, so a
/// plugin can't make the audio thread allocate.
pub(super) struct ReportFeed {
    /// To the editor.
    reports: Producer<ParamReport>,
    /// Nanoseconds per sample.
    ns_per_frame: f64,
    /// When the block began, on the monotonic timeline.
    block_ns: u64,
    /// Whether this block reported anything.
    reported: bool,
}

impl OutputEventBuffer for ReportFeed {
    /// Never refuses: a plugin may treat a failed push as an error, and an
    /// event a host has no use for isn't one. A report that finds the ring
    /// full is dropped.
    fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
        let (block_ns, ns_per_frame) = (self.block_ns, self.ns_per_frame);
        if let Some(report) = param_report(event, |time| event_nanos(block_ns, time, ns_per_frame))
        {
            if self.reports.push(report).is_err() {
                debug!("clap: parameter report ring full; dropped one");
            }
            self.reported = true;
        }
        Ok(())
    }
}

/// The voice's end of the bridge to the editor.
pub(super) struct VoiceParams {
    /// The plugin's parameters: a move is sent in its plain value.
    table: Arc<ParamTable>,
    /// Moves from the editor.
    moves: Consumer<(ParamId, f64)>,
    /// The output events of the block in hand.
    pub(super) feed: ReportFeed,
    /// Tells the UI there are reports to read.
    wake: Arc<UiWake>,
}

impl VoiceParams {
    /// Queues a move of parameter `id` to `value` (normalized) at sample
    /// `time`, in order among `events`. Returns whether it went in: a full
    /// list, or an ID the plugin doesn't have, drops it.
    pub(super) fn queue(
        &self,
        events: &mut EventBuffer,
        id: ParamId,
        value: f64,
        time: u32,
    ) -> bool {
        let (Some(param), Some(clap_id)) = (self.table.get(&id), ClapId::from_raw(id)) else {
            return false;
        };
        if events.len() as usize >= EVENT_CAPACITY {
            debug!("clap: event buffer full; dropped a move of {id}");
            return false;
        }
        events.push(&ParamValueEvent::new(
            time,
            clap_id,
            Pckn::match_all(),
            param.range.plain(value),
        ));
        true
    }

    /// Whether the editor's moves wait for a block: a sleeping voice must wake
    /// to deliver them.
    pub(super) fn moves_waiting(&self) -> bool {
        !self.moves.is_empty()
    }

    /// Adds the editor's moves to a block's `events`, at its first sample:
    /// a hand's turn has no finer timing. A move that finds the list full
    /// stays in the ring for the next block, so [`queue`](Self::queue) is
    /// only asked while there is room.
    pub(super) fn drain_moves(&mut self, events: &mut EventBuffer) {
        if self.moves.is_empty() {
            return;
        }
        while (events.len() as usize) < EVENT_CAPACITY {
            let Ok((id, value)) = self.moves.pop() else {
                break;
            };
            self.queue(events, id, value, 0);
        }
        // CLAP wants a block's events in time order, and these came last.
        events.sort();
    }

    /// A block begins at `now_ns`.
    pub(super) fn start_block(&mut self, now_ns: u64) {
        self.feed.block_ns = now_ns;
    }

    /// The block's `process` returned: wakes the UI if it has reports to read.
    pub(super) fn finish_block(&mut self) {
        if std::mem::take(&mut self.feed.reported) {
            self.wake.request();
        }
    }
}

/// The two ends of a plugin's parameter bridge, over `values`.
pub(super) fn bridge(
    values: ParamValues,
    sample_rate: f64,
    wake: Arc<UiWake>,
) -> (EditorParams, VoiceParams) {
    let (moves_tx, moves_rx) = RingBuffer::new(PARAM_RING_CAPACITY);
    let (reports_tx, reports_rx) = RingBuffer::new(PARAM_RING_CAPACITY);
    let table = Arc::clone(&values.table);
    (
        EditorParams {
            values,
            moves: moves_tx,
            reports: reports_rx,
        },
        VoiceParams {
            table,
            moves: moves_rx,
            feed: ReportFeed {
                reports: reports_tx,
                ns_per_frame: 1e9 / sample_rate,
                block_ns: 0,
                reported: false,
            },
            wake,
        },
    )
}

#[cfg(test)]
mod tests {
    use clack_host::events::Event;
    use clack_host::events::event_types::{
        MidiEvent, ParamGestureBeginEvent, ParamGestureEndEvent,
    };

    use super::*;

    /// A cutoff of 20–20000 standing at 20, default 1010, as ID 7, and a mode
    /// switch of 0–2 standing at 0, as ID 9.
    fn values() -> ParamValues {
        ParamValues::new([
            (
                7,
                ClapParam::new("Cutoff".to_owned(), 20.0, 20_000.0, 10_010.0, false),
                20.0,
            ),
            (
                9,
                ClapParam::new("Mode".to_owned(), 0.0, 2.0, 0.0, true),
                0.0,
            ),
        ])
    }

    fn id(id: u32) -> ClapId {
        ClapId::from_raw(id).unwrap()
    }

    fn value_report(param: ParamId, plain: f64, recorded: bool) -> ParamReport {
        ParamReport::Value {
            id: param,
            plain,
            at_ns: 5,
            recorded,
        }
    }

    #[test]
    fn a_plain_value_maps_to_its_share_of_the_range_and_back() {
        let range = ParamRange {
            min: -12.0,
            max: 12.0,
            stepped: false,
        };
        assert_eq!(range.normalized(-12.0), 0.0);
        assert_eq!(range.normalized(0.0), 0.5);
        assert_eq!(range.normalized(12.0), 1.0);
        assert_eq!(range.plain(0.0), -12.0);
        assert_eq!(range.plain(0.25), -6.0);
        assert_eq!(range.plain(1.0), 12.0);
    }

    #[test]
    fn a_value_outside_the_range_is_held_to_it() {
        let range = ParamRange {
            min: 0.0,
            max: 10.0,
            stepped: false,
        };
        assert_eq!(range.normalized(-1.0), 0.0);
        assert_eq!(range.normalized(11.0), 1.0);
        assert_eq!(range.plain(1.5), 10.0);
    }

    #[test]
    fn a_range_with_nothing_in_it_reads_as_its_only_value() {
        let range = ParamRange {
            min: 3.0,
            max: 3.0,
            stepped: false,
        };
        assert_eq!(range.normalized(3.0), 0.0);
        assert_eq!(range.plain(0.7), 3.0);
    }

    /// A knob's positions (`snap_to_steps`) are `k / steps`, and each has to
    /// reach the plugin as its own whole number.
    #[test]
    fn every_position_of_a_stepped_parameter_lands_on_its_whole_number() {
        for steps in 1..=128u32 {
            let range = ParamRange {
                min: -3.0,
                max: f64::from(steps) - 3.0,
                stepped: true,
            };
            assert_eq!(range.steps(), steps);
            for position in 0..=steps {
                let value = f64::from(position) / f64::from(steps);
                assert_eq!(range.plain(value), f64::from(position) - 3.0);
                assert_eq!(range.normalized(range.plain(value)), value);
            }
        }
    }

    #[test]
    fn a_smooth_parameter_has_no_steps() {
        let range = ParamRange {
            min: 0.0,
            max: 5.0,
            stepped: false,
        };
        assert_eq!(range.steps(), 0);
        assert_eq!(range.plain(0.31), 0.31 * 5.0);
    }

    #[test]
    fn a_parameters_default_is_normalized() {
        let values = values();
        let (name, default) = (
            &values.table[&7].info.name,
            values.table[&7].info.default_value,
        );
        assert_eq!(name, "Cutoff");
        assert_eq!(default, 0.5);
        assert_eq!(values.table[&9].info.steps, 2);
    }

    #[test]
    fn output_events_read_as_reports_at_their_sample() {
        let at = |time: u32| u64::from(time) + 1000;
        let begin = ParamGestureBeginEvent::new(10, id(7));
        assert_eq!(
            param_report(begin.as_ref(), at),
            Some(ParamReport::Begin(7, 1010))
        );
        let value = ParamValueEvent::new(20, id(7), Pckn::match_all(), 440.0);
        assert_eq!(
            param_report(value.as_ref(), at),
            Some(ParamReport::Value {
                id: 7,
                plain: 440.0,
                at_ns: 1020,
                recorded: true,
            })
        );
        let end = ParamGestureEndEvent::new(30, id(7));
        assert_eq!(param_report(end.as_ref(), at), Some(ParamReport::End(7)));
    }

    #[test]
    fn a_value_the_plugin_says_not_to_record_reads_as_such() {
        let value = ParamValueEvent::new(0, id(7), Pckn::match_all(), 1.0)
            .with_flags(EventFlags::DONT_RECORD);
        assert!(matches!(
            param_report(value.as_ref(), |_| 0),
            Some(ParamReport::Value {
                recorded: false,
                ..
            })
        ));
    }

    #[test]
    fn an_output_event_about_no_parameter_reports_nothing() {
        let midi = MidiEvent::new(0, 0, [0x90, 60, 100]);
        assert_eq!(param_report(midi.as_ref(), |_| 0), None);
    }

    #[test]
    fn an_events_sample_offset_moves_its_time() {
        // 48 kHz: 480 samples is 10 ms.
        assert_eq!(event_nanos(1_000, 480, 1e9 / 48_000.0), 10_001_000);
        assert_eq!(event_nanos(1_000, 0, 1e9 / 48_000.0), 1_000);
    }

    /// By the time the panel reads the gesture the plugin has moved on, so
    /// the start has to come from before the moves in the same batch.
    #[test]
    fn a_turns_start_carries_the_value_from_before_it() {
        let mut values = values();
        assert_eq!(
            values.gesture(ParamReport::Begin(7, 1)),
            Some(ParameterGesture::Begin(7, Some(0.0), 1))
        );
        assert_eq!(
            values.gesture(value_report(7, 10_010.0, true)),
            Some(ParameterGesture::Move(7, 0.5, 5))
        );
        assert_eq!(
            values.gesture(ParamReport::Begin(7, 9)),
            Some(ParameterGesture::Begin(7, Some(0.5), 9))
        );
        assert_eq!(
            values.gesture(ParamReport::End(7)),
            Some(ParameterGesture::End(7))
        );
    }

    #[test]
    fn a_value_that_changes_nothing_is_no_move() {
        let mut values = values();
        assert_eq!(values.gesture(value_report(7, 20.0, true)), None);
    }

    #[test]
    fn a_value_the_plugin_says_not_to_record_moves_only_the_knob() {
        let mut values = values();
        assert_eq!(values.gesture(value_report(7, 10_010.0, false)), None);
        assert_eq!(values.values[&7], 0.5);
    }

    #[test]
    fn a_report_of_a_parameter_the_plugin_never_listed_means_nothing() {
        let mut values = values();
        assert_eq!(values.gesture(ParamReport::Begin(1, 0)), None);
        assert_eq!(values.gesture(value_report(1, 1.0, true)), None);
        assert_eq!(values.gesture(ParamReport::End(1)), None);
    }

    #[test]
    fn a_rescan_reports_the_values_that_moved_and_no_others() {
        let mut values = values();
        let moved = values.reread(|id| Some(if id == 7 { 20_000.0 } else { 0.0 }), 3);
        assert_eq!(moved, vec![ParameterGesture::Move(7, 1.0, 3)]);
        assert_eq!(values.values[&7], 1.0);
    }

    fn sides() -> (EditorParams, VoiceParams) {
        bridge(values(), 48_000.0, Arc::new(UiWake::default()))
    }

    fn sent(events: &EventBuffer) -> Vec<(u32, f64)> {
        events
            .iter()
            .filter_map(|event| match event.as_core_event()? {
                CoreEventSpace::ParamValue(value) => Some((value.time(), value.value())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_move_from_the_editor_reaches_the_plugin_plain_and_shows_at_once() {
        let (mut editor, mut voice) = sides();
        editor.send(7, 0.5);
        assert_eq!(editor.value(7), Some(0.5));
        assert!(voice.moves_waiting());

        let mut events = EventBuffer::with_capacity(EVENT_CAPACITY);
        voice.drain_moves(&mut events);
        assert_eq!(sent(&events), vec![(0, 10_010.0)]);
        assert!(!voice.moves_waiting());
    }

    /// Stev's own move, repeated back by the plugin, isn't the hand's.
    #[test]
    fn a_move_stev_sent_is_not_read_back_as_a_turn() {
        let (mut editor, mut voice) = sides();
        editor.send(7, 0.5);
        let echo = ParamValueEvent::new(0, id(7), Pckn::match_all(), 10_010.0);
        voice.feed.try_push(echo.as_ref()).unwrap();
        assert!(editor.take_gestures().is_empty());
    }

    /// CLAP wants time order, and the mixer's clip events go in before the
    /// editor's moves. Two moves in one block keep theirs: the last one wins.
    #[test]
    fn the_editors_moves_go_ahead_of_a_blocks_clip_moves_in_their_own_order() {
        let (mut editor, mut voice) = sides();
        let mut events = EventBuffer::with_capacity(EVENT_CAPACITY);
        assert!(voice.queue(&mut events, 7, 1.0, 64));
        editor.send(9, 0.5);
        editor.send(9, 1.0);
        voice.drain_moves(&mut events);
        assert_eq!(sent(&events), vec![(0, 1.0), (0, 2.0), (64, 20_000.0)]);
    }

    #[test]
    fn a_move_of_a_parameter_the_plugin_never_listed_is_dropped() {
        let (_, voice) = sides();
        let mut events = EventBuffer::with_capacity(EVENT_CAPACITY);
        assert!(!voice.queue(&mut events, 1, 0.5, 0));
        assert!(events.is_empty());
    }

    #[test]
    fn a_full_block_leaves_the_editors_moves_for_the_next() {
        let (mut editor, mut voice) = sides();
        let mut events = EventBuffer::with_capacity(EVENT_CAPACITY);
        for _ in 0..EVENT_CAPACITY {
            assert!(voice.queue(&mut events, 7, 0.0, 0));
        }
        assert!(!voice.queue(&mut events, 7, 0.0, 0));

        editor.send(7, 1.0);
        voice.drain_moves(&mut events);
        assert!(voice.moves_waiting());
    }

    #[test]
    fn a_reported_block_wakes_the_ui_and_a_silent_one_does_not() {
        let wake = Arc::new(UiWake::default());
        let (mut editor, mut voice) = bridge(values(), 48_000.0, Arc::clone(&wake));

        voice.start_block(1_000);
        let midi = MidiEvent::new(0, 0, [0x90, 60, 100]);
        voice.feed.try_push(midi.as_ref()).unwrap();
        voice.finish_block();
        assert!(!wake.take());

        voice.start_block(2_000);
        let begin = ParamGestureBeginEvent::new(48, id(7));
        voice.feed.try_push(begin.as_ref()).unwrap();
        voice.finish_block();
        assert!(wake.take());
        assert_eq!(
            editor.take_gestures(),
            vec![ParameterGesture::Begin(7, Some(0.0), 1_002_000)]
        );
    }

    #[test]
    fn playback_moves_the_value_without_a_second_send() {
        let (mut editor, voice) = sides();
        editor.follow(7, 0.25);
        assert_eq!(editor.value(7), Some(0.25));
        assert!(!voice.moves_waiting());
    }
}
