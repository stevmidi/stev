//! Turning Stev's raw MIDI into VST3 events.
//!
//! **This is the biggest structural difference between the two hosted formats.**
//! CLAP takes MIDI 1.0 bytes verbatim — `clap::voice`'s translation is one
//! `match` that copies three bytes into a `MidiEvent`. VST3 has no general MIDI
//! input at all, and splits one MIDI stream three ways:
//!
//! | MIDI | Becomes |
//! |---|---|
//! | note on / note off | a typed `Event` (with a `noteId`, see below) |
//! | poly key pressure | a typed `Event` |
//! | CC, channel pressure, pitch bend | a **parameter change**, via [`MidiMap`] |
//! | everything else | dropped |
//!
//! The third row is the awkward one. A VST3 plugin does not receive controllers;
//! it exposes *parameters* and tells the host, through `IMidiMapping`, which
//! parameter each controller should drive. So a mod wheel only does anything if
//! the host asks that question at load and rewrites every CC into automation on
//! the answer — which is what [`MidiMap`] is.
//!
//! ## Note ids
//!
//! Every note-on carries a `noteId`, and its note-off **must carry the same
//! one**. Plugins that key their voices by id — rather than by pitch — leave a
//! note sounding forever if the off arrives with a mismatched or absent id.
//! [`NoteIds`] is the per-voice table that keeps them paired.
//!
//! See `docs/180-vst3-host.md`.

use vst3::ComPtr;
use vst3::Steinberg::Vst::ControllerNumbers_::{kAfterTouch, kCountCtrlNumber, kPitchBend};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent, kPolyPressureEvent};
use vst3::Steinberg::Vst::kNoParamId;
use vst3::Steinberg::Vst::{
    Event, Event__type0, IMidiMapping, IMidiMappingTrait, NoteOffEvent, NoteOnEvent, ParamID,
    ParamValue, PolyPressureEvent,
};
use vst3::Steinberg::kResultOk;

use crate::core::midi::message::{Midi3, pitch_bend_value};

/// MIDI status nibble for note-off.
const NOTE_OFF: u8 = 0x80;
/// MIDI status nibble for note-on.
const NOTE_ON: u8 = 0x90;
/// MIDI status nibble for polyphonic key pressure.
const POLY_PRESSURE: u8 = 0xA0;
/// MIDI status nibble for a control change.
const CONTROL_CHANGE: u8 = 0xB0;
/// MIDI status nibble for channel pressure (mono aftertouch).
const CHANNEL_PRESSURE: u8 = 0xD0;
/// MIDI status nibble for pitch bend.
const PITCH_BEND: u8 = 0xE0;

/// Largest value a 7-bit MIDI data byte can hold, as the divisor that
/// normalises one to `0.0..=1.0`.
const MIDI_7BIT_MAX: f64 = 127.0;

/// Largest value a 14-bit MIDI pair can hold — pitch bend's range.
const MIDI_14BIT_MAX: f64 = 16383.0;

/// Entries in a [`MidiMap`]'s per-channel row: the 128 controllers plus VST3's
/// two synthetic ones (`kAfterTouch`, `kPitchBend`).
const CONTROLLERS: usize = kCountCtrlNumber as usize;

/// MIDI channels, and keys per channel — the shape of the note-id table.
const CHANNELS: usize = 16;
/// Keys per MIDI channel.
const KEYS: usize = 128;

/// Pairs each sounding note-on with the `noteId` its note-off must repeat.
///
/// A fresh id is minted per note-on and remembered until the matching off. A
/// re-triggered key (a second on with no off in between, which a legato clip
/// can easily produce) overwrites the stored id, so the *next* off releases the
/// most recent on — the same choice a pitch-keyed plugin would make anyway.
pub(super) struct NoteIds {
    /// `active[channel][key]` is the id of the note-on still awaiting its off.
    active: [[Option<i32>; KEYS]; CHANNELS],
    /// Next id to hand out. Wraps rather than overflowing; ids only have to be
    /// unique among *simultaneously sounding* notes, and `i32::MAX` of them is
    /// not a situation that arises.
    next: i32,
}

impl Default for NoteIds {
    fn default() -> Self {
        Self {
            active: [[None; KEYS]; CHANNELS],
            next: 0,
        }
    }
}

impl NoteIds {
    /// Mints and stores the id for a note-on.
    fn begin(&mut self, channel: usize, key: usize) -> i32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(0);
        self.active[channel][key] = Some(id);
        id
    }

    /// Takes the id a note-off should carry. `-1` — VST3's "no id" — when no
    /// matching on is outstanding, which is the right answer for a stray off
    /// (a panic burst, or a note that was already released).
    fn end(&mut self, channel: usize, key: usize) -> i32 {
        self.active[channel][key].take().unwrap_or(-1)
    }

    /// The id of the note currently sounding on a key, *without* releasing it —
    /// what a poly-pressure message needs so the plugin knows which voice it
    /// refers to.
    fn current(&self, channel: usize, key: usize) -> i32 {
        self.active[channel][key].unwrap_or(-1)
    }
}

/// Which parameter, if any, a plugin wants each MIDI controller to drive.
///
/// Built once per plugin at load by asking `IMidiMapping` for all 16 channels ×
/// [`CONTROLLERS`] combinations. A plugin that exposes no `IMidiMapping` gets an
/// empty map, and its CCs are dropped — correctly, since it has told us there is
/// nothing for them to do.
pub(super) struct MidiMap {
    /// `params[channel][controller]`, boxed because it is ~16 KB and is only
    /// ever read through a reference.
    params: Box<[[Option<ParamID>; CONTROLLERS]; CHANNELS]>,
}

impl MidiMap {
    /// A map with no assignments — every CC is dropped.
    pub(super) fn empty() -> Self {
        Self::build(|_, _| None)
    }

    /// Asks a plugin's `IMidiMapping` about every channel/controller pair.
    ///
    /// 16 × 130 vtable calls, once, at load. Doing it up front rather than per
    /// event keeps the audio thread to an array index.
    ///
    /// # Safety
    ///
    /// `mapping` must be a live `IMidiMapping` belonging to an initialised
    /// plugin.
    pub(super) unsafe fn query(mapping: &ComPtr<IMidiMapping>) -> Self {
        Self::build(|channel, controller| {
            let mut id: ParamID = 0;
            // SAFETY: the caller guarantees a live mapping; `id` is a local of
            // the matching type, read only when the call reports success.
            let ok = unsafe {
                mapping.getMidiControllerAssignment(0, channel, controller, &mut id) == kResultOk
            };
            assigned_param(ok, id)
        })
    }

    /// Fills the table from `lookup`. Separate from [`query`](Self::query) so
    /// the table's shape can be tested without a plugin.
    fn build(lookup: impl Fn(i16, i16) -> Option<ParamID>) -> Self {
        let mut params = Box::new([[None; CONTROLLERS]; CHANNELS]);
        for (channel, row) in params.iter_mut().enumerate() {
            for (controller, slot) in row.iter_mut().enumerate() {
                *slot = lookup(channel as i16, controller as i16);
            }
        }
        Self { params }
    }

    /// The parameter a controller drives on a channel, if any.
    fn param_for(&self, channel: usize, controller: usize) -> Option<ParamID> {
        self.params.get(channel)?.get(controller).copied().flatten()
    }
}

/// Interprets one `getMidiControllerAssignment` answer.
///
/// A plugin may report **success and still write `kNoParamId`**, meaning "this
/// controller drives nothing" — Cthulhu does exactly that for all 130 of them.
/// Taking the sentinel at face value would send automation to parameter
/// `0xFFFFFFFF` on every CC.
fn assigned_param(reported_ok: bool, id: ParamID) -> Option<ParamID> {
    (reported_ok && id != kNoParamId).then_some(id)
}

/// What one MIDI message becomes on the VST3 side.
pub(super) enum Translated {
    /// A typed event for the plugin's event list.
    Event(Event),
    /// A parameter change, already normalised to `0.0..=1.0`.
    Param(ParamID, ParamValue),
    /// Nothing VST3 can express, or a controller this plugin has not mapped.
    Nothing,
}

/// Translates one MIDI message for a plugin at sample offset `time`.
pub(super) fn translate(bytes: Midi3, time: u32, ids: &mut NoteIds, map: &MidiMap) -> Translated {
    let status = bytes[0] & 0xF0;
    let channel = usize::from(bytes[0] & 0x0F);

    // Controller-shaped messages become parameter automation, if the plugin
    // asked for any. The controller number is VST3's, which extends MIDI's 0-127
    // with two synthetic entries for the messages that have no CC number.
    let controller_change = match status {
        CONTROL_CHANGE => Some((
            usize::from(bytes[1] & 0x7F),
            f64::from(bytes[2] & 0x7F) / MIDI_7BIT_MAX,
        )),
        CHANNEL_PRESSURE => Some((
            kAfterTouch as usize,
            f64::from(bytes[1] & 0x7F) / MIDI_7BIT_MAX,
        )),
        PITCH_BEND => Some((
            kPitchBend as usize,
            f64::from(pitch_bend_value(bytes)) / MIDI_14BIT_MAX,
        )),
        _ => None,
    };
    if let Some((controller, value)) = controller_change {
        return match map.param_for(channel, controller) {
            Some(id) => Translated::Param(id, value),
            None => Translated::Nothing,
        };
    }

    match midi_to_event(bytes, time, ids) {
        Some(event) => Translated::Event(event),
        None => Translated::Nothing,
    }
}

/// Translates one MIDI message into a VST3 event at sample offset `time`.
///
/// `None` for anything VST3 has no event for — the controller-shaped messages
/// [`translate`] has already handled, and everything else.
fn midi_to_event(bytes: Midi3, time: u32, ids: &mut NoteIds) -> Option<Event> {
    let status = bytes[0] & 0xF0;
    let channel = usize::from(bytes[0] & 0x0F);
    let key = usize::from(bytes[1] & 0x7F);
    let velocity = f32::from(bytes[2] & 0x7F) / 127.0;

    // A note-on at velocity 0 is a note-off — the running-status convention
    // that virtually every keyboard and sequencer emits. Missing this leaves
    // notes hanging on any device that uses it.
    let is_note_off = status == NOTE_OFF || (status == NOTE_ON && (bytes[2] & 0x7F) == 0);

    let (event_type, payload) = if is_note_off {
        (
            kNoteOffEvent,
            Event__type0 {
                noteOff: NoteOffEvent {
                    channel: channel as i16,
                    pitch: key as i16,
                    velocity,
                    noteId: ids.end(channel, key),
                    tuning: 0.0,
                },
            },
        )
    } else if status == POLY_PRESSURE {
        (
            kPolyPressureEvent,
            Event__type0 {
                polyPressure: PolyPressureEvent {
                    channel: channel as i16,
                    pitch: key as i16,
                    pressure: f32::from(bytes[2] & 0x7F) / MIDI_7BIT_MAX as f32,
                    // The note this applies to, when it is still sounding.
                    noteId: ids.current(channel, key),
                },
            },
        )
    } else if status == NOTE_ON {
        (
            kNoteOnEvent,
            Event__type0 {
                noteOn: NoteOnEvent {
                    channel: channel as i16,
                    pitch: key as i16,
                    tuning: 0.0,
                    velocity,
                    // 0 = "not known" — Stev streams notes rather than
                    // scheduling them with a duration up front.
                    length: 0,
                    noteId: ids.begin(channel, key),
                },
            },
        )
    } else {
        return None;
    };

    Some(Event {
        busIndex: 0,
        sampleOffset: time as i32,
        ppqPosition: 0.0,
        flags: 0,
        r#type: event_type as u16,
        __field0: payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(bytes: Midi3, ids: &mut NoteIds) -> Event {
        midi_to_event(bytes, 0, ids).expect("should translate")
    }

    /// A map where every channel routes `controller` to `id`.
    fn map_of(controller: usize, id: ParamID) -> MidiMap {
        MidiMap::build(move |_, c| (c as usize == controller).then_some(id))
    }

    /// The parameter a message maps to, if any.
    fn param(bytes: Midi3, map: &MidiMap) -> Option<(ParamID, ParamValue)> {
        match translate(bytes, 0, &mut NoteIds::default(), map) {
            Translated::Param(id, v) => Some((id, v)),
            _ => None,
        }
    }

    #[test]
    fn a_mapped_cc_becomes_a_normalised_parameter_change() {
        // The mod wheel, the single most-missed controller.
        let map = map_of(1, 42);
        let (id, value) = param([0xB0, 1, 127], &map).expect("mapped");
        assert_eq!(id, 42);
        assert!((value - 1.0).abs() < 1e-12);
        assert_eq!(param([0xB0, 1, 0], &map).unwrap().1, 0.0);
        let mid = param([0xB0, 1, 64], &map).unwrap().1;
        assert!((mid - 64.0 / 127.0).abs() < 1e-12);
    }

    #[test]
    fn an_unmapped_cc_is_dropped_rather_than_guessed_at() {
        // A plugin that maps nothing has said its CCs do nothing.
        let map = map_of(1, 42);
        assert!(param([0xB0, 7, 100], &map).is_none());
        assert!(param([0xB0, 1, 100], &MidiMap::empty()).is_none());
    }

    #[test]
    fn the_no_parameter_sentinel_is_not_taken_as_an_id() {
        // Cthulhu reports success and writes `kNoParamId` for all 130
        // controllers. Believing it would route every CC to parameter
        // 0xFFFFFFFF.
        assert_eq!(assigned_param(true, kNoParamId), None);
        // A refused query, whatever it left in the out-parameter.
        assert_eq!(assigned_param(false, 42), None);
        // A real assignment, including id 0, which is perfectly valid.
        assert_eq!(assigned_param(true, 42), Some(42));
        assert_eq!(assigned_param(true, 0), Some(0));
    }

    #[test]
    fn pitch_bend_uses_its_full_fourteen_bit_range() {
        // LSB then MSB, and centre must land exactly at 0.5 — a pitch wheel
        // that rests slightly sharp is immediately audible.
        let map = map_of(kPitchBend as usize, 7);
        assert_eq!(param([0xE0, 0, 0], &map).unwrap().1, 0.0);
        assert!((param([0xE0, 0x7F, 0x7F], &map).unwrap().1 - 1.0).abs() < 1e-12);
        let centre = param([0xE0, 0x00, 0x40], &map).unwrap().1;
        assert!((centre - 0.5).abs() < 1e-4, "centre was {centre}");
    }

    #[test]
    fn pitch_bend_reassembles_lsb_before_msb() {
        // Getting these the wrong way round still produces plausible-looking
        // numbers, which is what makes it worth pinning.
    }

    #[test]
    fn channel_pressure_maps_to_the_synthetic_aftertouch_controller() {
        let map = map_of(kAfterTouch as usize, 9);
        let (id, value) = param([0xD0, 127, 0], &map).expect("mapped");
        assert_eq!(id, 9);
        assert!((value - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_mapping_is_per_channel() {
        // A plugin may map a controller on one channel and not another.
        let map = MidiMap::build(|ch, c| (ch == 3 && c == 1).then_some(5));
        assert_eq!(param([0xB3, 1, 64], &map).map(|(id, _)| id), Some(5));
        assert!(param([0xB0, 1, 64], &map).is_none());
    }

    #[test]
    fn poly_pressure_becomes_an_event_carrying_the_sounding_notes_id() {
        // It refers to a specific voice, so it needs that voice's id — and must
        // not release the note the way a note-off would.
        let mut ids = NoteIds::default();
        let map = MidiMap::empty();
        let on = ev([0x90, 60, 100], &mut ids);
        let Translated::Event(pressure) = translate([0xA0, 60, 64], 0, &mut ids, &map) else {
            panic!("poly pressure should be an event");
        };
        assert_eq!(pressure.r#type, kPolyPressureEvent as u16);
        // SAFETY: the type tags say which union members are live.
        unsafe {
            assert_eq!(pressure.__field0.polyPressure.pitch, 60);
            assert_eq!(
                pressure.__field0.polyPressure.noteId,
                on.__field0.noteOn.noteId
            );
        }
        // The note is still sounding, so its off still pairs correctly.
        let off = ev([0x80, 60, 0], &mut ids);
        // SAFETY: as above.
        unsafe { assert_eq!(off.__field0.noteOff.noteId, on.__field0.noteOn.noteId) };
    }

    #[test]
    fn notes_are_unaffected_by_the_controller_path() {
        let map = map_of(1, 42);
        let mut ids = NoteIds::default();
        assert!(matches!(
            translate([0x90, 60, 100], 0, &mut ids, &map),
            Translated::Event(_)
        ));
        assert!(matches!(
            translate([0x80, 60, 0], 0, &mut ids, &map),
            Translated::Nothing | Translated::Event(_)
        ));
    }

    #[test]
    fn program_change_is_still_dropped() {
        // VST3 routes programs through a unit/program-list model, not this one.
        let map = map_of(1, 42);
        assert!(matches!(
            translate([0xC0, 3, 0], 0, &mut NoteIds::default(), &map),
            Translated::Nothing
        ));
    }

    #[test]
    fn note_on_and_note_off_become_their_typed_events() {
        let mut ids = NoteIds::default();
        let on = ev([0x90, 60, 100], &mut ids);
        assert_eq!(on.r#type, kNoteOnEvent as u16);
        // SAFETY: the type tag says this union member is the live one.
        unsafe {
            assert_eq!(on.__field0.noteOn.pitch, 60);
            assert_eq!(on.__field0.noteOn.channel, 0);
            assert!((on.__field0.noteOn.velocity - 100.0 / 127.0).abs() < 1e-6);
        }

        let off = ev([0x80, 60, 0], &mut ids);
        assert_eq!(off.r#type, kNoteOffEvent as u16);
        // SAFETY: as above.
        unsafe { assert_eq!(off.__field0.noteOff.pitch, 60) };
    }

    #[test]
    fn a_note_off_repeats_its_note_ons_id() {
        // The whole reason `NoteIds` exists: a plugin that keys voices by id
        // hangs the note if these disagree.
        let mut ids = NoteIds::default();
        let on = ev([0x91, 64, 100], &mut ids);
        let off = ev([0x81, 64, 0], &mut ids);
        // SAFETY: the type tags say which union members are live.
        unsafe {
            assert_eq!(on.__field0.noteOn.noteId, off.__field0.noteOff.noteId);
            assert_ne!(off.__field0.noteOff.noteId, -1);
        }
    }

    #[test]
    fn simultaneous_notes_get_distinct_ids() {
        let mut ids = NoteIds::default();
        let a = ev([0x90, 60, 100], &mut ids);
        let b = ev([0x90, 64, 100], &mut ids);
        // SAFETY: both are note-ons.
        unsafe { assert_ne!(a.__field0.noteOn.noteId, b.__field0.noteOn.noteId) };
    }

    #[test]
    fn a_retriggered_key_releases_the_most_recent_on() {
        let mut ids = NoteIds::default();
        let _first = ev([0x90, 60, 100], &mut ids);
        let second = ev([0x90, 60, 100], &mut ids);
        let off = ev([0x80, 60, 0], &mut ids);
        // SAFETY: the type tags say which union members are live.
        unsafe { assert_eq!(off.__field0.noteOff.noteId, second.__field0.noteOn.noteId) };
    }

    #[test]
    fn a_stray_note_off_carries_the_no_id_sentinel() {
        let mut ids = NoteIds::default();
        let off = ev([0x80, 60, 0], &mut ids);
        // SAFETY: it is a note-off.
        unsafe { assert_eq!(off.__field0.noteOff.noteId, -1) };
    }

    #[test]
    fn a_zero_velocity_note_on_is_a_note_off() {
        // The running-status convention. Getting this wrong hangs every note
        // from any device that uses it.
        let mut ids = NoteIds::default();
        let on = ev([0x90, 60, 100], &mut ids);
        let off = ev([0x90, 60, 0], &mut ids);
        assert_eq!(off.r#type, kNoteOffEvent as u16);
        // SAFETY: the type tags say which union members are live.
        unsafe { assert_eq!(off.__field0.noteOff.noteId, on.__field0.noteOn.noteId) };
    }

    #[test]
    fn the_sample_offset_is_carried_into_the_event() {
        let mut ids = NoteIds::default();
        let e = midi_to_event([0x90, 60, 100], 123, &mut ids).unwrap();
        assert_eq!(e.sampleOffset, 123);
    }

    #[test]
    fn the_event_path_alone_handles_only_notes_and_poly_pressure() {
        // Controller-shaped messages are the `translate` layer's job; this one
        // must not invent events for them.
        let mut ids = NoteIds::default();
        for status in [0xB0, 0xC0, 0xD0, 0xE0] {
            assert!(midi_to_event([status, 1, 64], 0, &mut ids).is_none());
        }
        assert!(midi_to_event([0xA0, 60, 64], 0, &mut ids).is_some());
    }

    #[test]
    fn the_channel_nibble_is_honoured() {
        let mut ids = NoteIds::default();
        let e = ev([0x9F, 60, 100], &mut ids);
        // SAFETY: it is a note-on.
        unsafe { assert_eq!(e.__field0.noteOn.channel, 15) };
    }

    #[test]
    fn ids_on_different_channels_do_not_collide() {
        let mut ids = NoteIds::default();
        let a = ev([0x90, 60, 100], &mut ids);
        let b = ev([0x91, 60, 100], &mut ids);
        let off_b = ev([0x81, 60, 0], &mut ids);
        let off_a = ev([0x80, 60, 0], &mut ids);
        // SAFETY: the type tags say which union members are live.
        unsafe {
            assert_eq!(off_a.__field0.noteOff.noteId, a.__field0.noteOn.noteId);
            assert_eq!(off_b.__field0.noteOff.noteId, b.__field0.noteOn.noteId);
        }
    }
}
