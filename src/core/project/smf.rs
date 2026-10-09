//! Standard MIDI Files (`.mid`), both ways, hand-rolled (no dependency).
//! Pure bytes in, bytes out.
//!
//! - **Write** — the clip export (`⌘/Ctrl+⇧+E`, `060-persistence.md` § MIDI
//!   clip export): one Type 0 file with the app's `PPQN` as its division, a
//!   tempo and a 4/4 time signature up front, the channel messages, and the
//!   end-of-track at the clip's length so another DAW imports the loop at its
//!   full length.
//! - **Read** — the clip import (`060` § MIDI clip import): every track of a
//!   Type 0/1/2 file merged into one stream of channel messages, timed in the
//!   app's `PPQN`. Meta events (tempo included — the project's tempo stays)
//!   and sysex are skipped.

use std::fmt;

use crate::{
    core::{midi::message::is_channel_message, time::PPQN},
    models::event::Event,
};

/// The bytes of a Type 0 SMF holding `events` (sorted by tick, ticks from
/// the file start) at `tempo_us` microseconds per beat, its end-of-track at
/// `length` ticks (or the last event, if later). Only channel messages are
/// written: system messages (sysex, realtime) have no place in a clip export.
pub(crate) fn write_smf(events: &[Event], length: i32, tempo_us: i32) -> Vec<u8> {
    let mut track = Vec::new();

    // Tempo: FF 51 03 tt tt tt (24-bit µs per quarter note).
    let tempo = tempo_us.clamp(1, 0xFF_FFFF).to_be_bytes();
    track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, tempo[1], tempo[2], tempo[3]]);
    // Time signature 4/4: FF 58 04 nn dd cc bb — numerator, denominator as
    // a power of two, 24 MIDI clocks per click, 8 32nds per quarter.
    track.extend_from_slice(&[0x00, 0xFF, 0x58, 0x04, 4, 2, 24, 8]);

    let mut last_tick = 0;
    for event in events {
        let message = event.midi_message();
        if !is_channel_message(message) {
            continue;
        }
        let tick = event.tick().max(last_tick);
        write_vlq(&mut track, tick - last_tick);
        track.extend_from_slice(message);
        last_tick = tick;
    }

    write_vlq(&mut track, (length - last_tick).max(0));
    track.extend_from_slice(&[0xFF, 0x2F, 0x00]);

    let mut file = Vec::with_capacity(14 + 8 + track.len());
    file.extend_from_slice(b"MThd");
    file.extend_from_slice(&6_u32.to_be_bytes());
    file.extend_from_slice(&0_u16.to_be_bytes()); // format 0
    file.extend_from_slice(&1_u16.to_be_bytes()); // one track
    file.extend_from_slice(&(PPQN as u16).to_be_bytes());
    file.extend_from_slice(b"MTrk");
    file.extend_from_slice(&(track.len() as u32).to_be_bytes());
    file.extend_from_slice(&track);
    file
}

/// What [`read_smf`] found: the channel messages of every track, merged.
#[derive(Debug)]
pub(crate) struct SmfContents {
    /// Every channel message, in `PPQN` ticks from the file start, sorted by
    /// tick (stable: file track order, then file order). A zero-velocity
    /// `NoteOn` comes out as a `NoteOff`.
    pub(crate) events: Vec<Event>,
    /// The latest end-of-track (or event) of any track, in `PPQN` ticks.
    pub(crate) end_tick: i32,
}

/// Why a `.mid` could not be read.
#[derive(Debug, PartialEq)]
pub(crate) enum SmfError {
    /// No `MThd` header: not a Standard MIDI File.
    NotMidi,
    /// SMPTE time division (frames per second, not ticks per beat) — rare
    /// outside film work, and nothing in the app is timed in frames.
    SmpteTiming,
    /// A data byte where a status byte must be, or a truncated header.
    Malformed,
}

impl fmt::Display for SmfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SmfError::NotMidi => "not a MIDI file",
            SmfError::SmpteTiming => "SMPTE-timed MIDI files are not supported",
            SmfError::Malformed => "the MIDI file is damaged",
        })
    }
}

/// Reads a Standard MIDI File: every `MTrk` chunk's channel messages merged
/// into one stream, rescaled from the file's ticks per beat to `PPQN`
/// (rounded to the nearest tick). Lenient where a reader can be: unknown
/// chunks are skipped, a chunk whose length runs past the end of the file is
/// read as far as it goes, and a track without an end-of-track ends at its
/// last event.
pub(crate) fn read_smf(bytes: &[u8]) -> Result<SmfContents, SmfError> {
    let mut reader = ByteReader::new(bytes);
    if reader.take(4) != Some(b"MThd") {
        return Err(SmfError::NotMidi);
    }
    let header_len = reader.u32().ok_or(SmfError::Malformed)? as usize;
    let header = reader.take(header_len).ok_or(SmfError::Malformed)?;
    if header.len() < 6 {
        return Err(SmfError::Malformed);
    }
    let division = u16::from_be_bytes([header[4], header[5]]);
    if division & 0x8000 != 0 {
        return Err(SmfError::SmpteTiming);
    }
    let ticks_per_beat = i64::from(division.max(1));
    let rescale = |tick: u32| -> i32 {
        let scaled = (i64::from(tick) * i64::from(PPQN) + ticks_per_beat / 2) / ticks_per_beat;
        scaled.min(i64::from(i32::MAX)) as i32
    };

    let mut events = Vec::new();
    let mut end_tick = 0;
    while let (Some(kind), Some(len)) = (reader.take(4), reader.u32()) {
        let chunk = reader.take_up_to(len as usize);
        if kind != b"MTrk" {
            continue;
        }
        let track_end = read_track(chunk, |tick, message| {
            events.push(Event::new(rescale(tick), 0, message));
        })?;
        end_tick = end_tick.max(rescale(track_end));
    }

    // Stable, so each track's own order survives on a shared tick.
    events.sort_by_key(Event::tick);
    Ok(SmfContents { events, end_tick })
}

/// Walks one `MTrk` chunk's body, handing each channel message (with its
/// absolute file tick) to `emit`. Returns the track's end tick: its
/// end-of-track meta, or its last event without one.
fn read_track(chunk: &[u8], mut emit: impl FnMut(u32, Vec<u8>)) -> Result<u32, SmfError> {
    let mut reader = ByteReader::new(chunk);
    let mut tick: u32 = 0;
    let mut running_status: Option<u8> = None;

    while !reader.is_empty() {
        let Some(delta) = reader.vlq() else {
            break;
        };
        tick = tick.saturating_add(delta);
        let Some(first) = reader.byte() else {
            break;
        };

        let status = if first & 0x80 != 0 {
            first
        } else {
            // Running status: `first` is the first data byte.
            running_status.ok_or(SmfError::Malformed)?
        };

        match status {
            0xFF => {
                let (Some(kind), Some(len)) = (reader.byte(), reader.vlq()) else {
                    break;
                };
                reader.take_up_to(len as usize);
                if kind == 0x2F {
                    return Ok(tick);
                }
            }
            0xF0 | 0xF7 => {
                // Sysex (or an escape): skipped. Cancels running status.
                running_status = None;
                let Some(len) = reader.vlq() else {
                    break;
                };
                reader.take_up_to(len as usize);
            }
            0xF1..=0xFE => return Err(SmfError::Malformed),
            _ => {
                running_status = Some(status);
                let data_len = if matches!(status & 0xF0, 0xC0 | 0xD0) {
                    1
                } else {
                    2
                };
                let mut message = vec![status];
                if first & 0x80 == 0 {
                    message.push(first);
                }
                while message.len() < 1 + data_len {
                    let Some(byte) = reader.byte() else {
                        return Ok(tick);
                    };
                    message.push(byte);
                }
                if !is_channel_message(&message) {
                    return Err(SmfError::Malformed);
                }
                if status & 0xF0 == 0x90 && message[2] == 0 {
                    message[0] = 0x80 | (status & 0x0F);
                }
                emit(tick, message);
            }
        }
    }
    Ok(tick)
}

/// A cursor over a byte slice for [`read_smf`]: every read is bounds-checked
/// and returns `None` past the end.
struct ByteReader<'a> {
    /// The bytes not read yet.
    rest: &'a [u8],
}

impl<'a> ByteReader<'a> {
    /// A reader at the start of `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// Whether everything has been read.
    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// The next byte.
    fn byte(&mut self) -> Option<u8> {
        let (&first, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(first)
    }

    /// The next `len` bytes, or `None` (reading nothing) if fewer are left.
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.rest.len() < len {
            return None;
        }
        let (taken, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(taken)
    }

    /// The next `len` bytes, or all that is left if fewer.
    fn take_up_to(&mut self, len: usize) -> &'a [u8] {
        let (taken, rest) = self.rest.split_at(len.min(self.rest.len()));
        self.rest = rest;
        taken
    }

    /// A big-endian `u32`.
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A variable-length quantity (at most four bytes, as the spec allows).
    fn vlq(&mut self) -> Option<u32> {
        let mut value: u32 = 0;
        for _ in 0..4 {
            let byte = self.byte()?;
            value = (value << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

/// Appends `value` as an SMF variable-length quantity: 7 bits per byte, most
/// significant first, the high bit set on every byte but the last. Negative
/// values write as `0`.
fn write_vlq(out: &mut Vec<u8>, value: i32) {
    let mut value = value.max(0) as u32;
    let mut bytes = [0_u8; 5];
    let mut idx = bytes.len() - 1;
    bytes[idx] = (value & 0x7F) as u8;
    value >>= 7;
    while value > 0 {
        idx -= 1;
        bytes[idx] = (value & 0x7F) as u8 | 0x80;
        value >>= 7;
    }
    out.extend_from_slice(&bytes[idx..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vlq(value: i32) -> Vec<u8> {
        let mut out = Vec::new();
        write_vlq(&mut out, value);
        out
    }

    /// The track chunk's body (after `MTrk` and its length).
    fn track_body(file: &[u8]) -> &[u8] {
        &file[22..]
    }

    /// The track body after the tempo (7 bytes) and time signature (8).
    fn after_meta(file: &[u8]) -> &[u8] {
        &track_body(file)[15..]
    }

    #[test]
    fn vlq_matches_the_spec_examples() {
        assert_eq!(vlq(0), [0x00]);
        assert_eq!(vlq(0x40), [0x40]);
        assert_eq!(vlq(0x7F), [0x7F]);
        assert_eq!(vlq(0x80), [0x81, 0x00]);
        assert_eq!(vlq(0x2000), [0xC0, 0x00]);
        assert_eq!(vlq(0x3FFF), [0xFF, 0x7F]);
        assert_eq!(vlq(0x4000), [0x81, 0x80, 0x00]);
        assert_eq!(vlq(0x0FFF_FFFF), [0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn vlq_writes_negative_as_zero() {
        assert_eq!(vlq(-5), [0x00]);
    }

    #[test]
    fn header_is_type_0_one_track_at_ppqn() {
        let file = write_smf(&[], PPQN * 4, 500_000);
        assert_eq!(&file[0..4], b"MThd");
        assert_eq!(&file[4..8], &[0, 0, 0, 6]);
        assert_eq!(&file[8..10], &[0, 0]);
        assert_eq!(&file[10..12], &[0, 1]);
        assert_eq!(&file[12..14], &(PPQN as u16).to_be_bytes());
        assert_eq!(&file[14..18], b"MTrk");
        let len = u32::from_be_bytes(file[18..22].try_into().unwrap()) as usize;
        assert_eq!(len, file.len() - 22);
    }

    #[test]
    fn empty_clip_writes_tempo_time_signature_and_end_at_its_length() {
        let file = write_smf(&[], PPQN * 4, 500_000);
        let mut expected = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20];
        expected.extend_from_slice(&[0x00, 0xFF, 0x58, 0x04, 4, 2, 24, 8]);
        expected.extend_from_slice(&vlq(PPQN * 4));
        expected.extend_from_slice(&[0xFF, 0x2F, 0x00]);
        assert_eq!(track_body(&file), expected);
    }

    #[test]
    fn notes_are_written_with_delta_times() {
        let events = [
            Event::new(0, 0, vec![0x90, 60, 100]),
            Event::new(PPQN, 0, vec![0x80, 60, 0]),
            Event::new(PPQN, 0, vec![0x91, 64, 90]),
            Event::new(PPQN * 2, 0, vec![0x81, 64, 0]),
        ];
        let file = write_smf(&events, PPQN * 4, 500_000);
        let notes = after_meta(&file);
        let mut expected = vec![0x00, 0x90, 60, 100];
        expected.extend(vlq(PPQN));
        expected.extend([0x80, 60, 0]);
        expected.extend([0x00, 0x91, 64, 90]);
        expected.extend(vlq(PPQN));
        expected.extend([0x81, 64, 0]);
        expected.extend(vlq(PPQN * 2));
        expected.extend([0xFF, 0x2F, 0x00]);
        assert_eq!(notes, expected);
    }

    #[test]
    fn end_of_track_never_precedes_the_last_event() {
        let events = [Event::new(PPQN * 8, 0, vec![0x80, 60, 0])];
        let file = write_smf(&events, PPQN * 4, 500_000);
        assert!(track_body(&file).ends_with(&[0x00, 0xFF, 0x2F, 0x00]));
    }

    #[test]
    fn system_and_malformed_messages_are_skipped() {
        let events = [
            Event::new(0, 0, vec![0xF8]),
            Event::new(0, 0, vec![0xF0, 0x7E, 0xF7]),
            Event::new(0, 0, vec![0x90, 60]),
            Event::new(0, 0, vec![0xB0, 64, 127]),
            Event::new(0, 0, vec![0xC0, 5]),
        ];
        let file = write_smf(&events, 0, 500_000);
        let notes = after_meta(&file);
        assert_eq!(
            notes,
            [0x00, 0xB0, 64, 127, 0x00, 0xC0, 5, 0x00, 0xFF, 0x2F, 0x00]
        );
    }

    #[test]
    fn tempo_is_written_as_24_bit_microseconds() {
        let file = write_smf(&[], 0, 600_000);
        assert_eq!(&track_body(&file)[4..7], &[0x09, 0x27, 0xC0]);
    }

    /// A file with `division` ticks per beat holding `tracks` (each a raw
    /// `MTrk` body).
    fn smf(format: u16, division: u16, tracks: &[&[u8]]) -> Vec<u8> {
        let mut file = b"MThd".to_vec();
        file.extend_from_slice(&6_u32.to_be_bytes());
        file.extend_from_slice(&format.to_be_bytes());
        file.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        file.extend_from_slice(&division.to_be_bytes());
        for track in tracks {
            file.extend_from_slice(b"MTrk");
            file.extend_from_slice(&(track.len() as u32).to_be_bytes());
            file.extend_from_slice(track);
        }
        file
    }

    /// `(tick, message)` of every event read.
    fn read_ticks(file: &[u8]) -> (Vec<(i32, Vec<u8>)>, i32) {
        let contents = read_smf(file).unwrap();
        let events = contents
            .events
            .iter()
            .map(|e| (e.tick(), e.midi_message().to_vec()))
            .collect();
        (events, contents.end_tick)
    }

    #[test]
    fn reads_back_what_the_writer_wrote() {
        let events = [
            Event::new(0, 0, vec![0x90, 60, 100]),
            Event::new(PPQN, 0, vec![0xB0, 64, 127]),
            Event::new(PPQN * 2, 0, vec![0x80, 60, 0]),
        ];
        let (read, end) = read_ticks(&write_smf(&events, PPQN * 4, 500_000));
        assert_eq!(
            read,
            vec![
                (0, vec![0x90, 60, 100]),
                (PPQN, vec![0xB0, 64, 127]),
                (PPQN * 2, vec![0x80, 60, 0]),
            ]
        );
        assert_eq!(end, PPQN * 4);
    }

    #[test]
    fn running_status_and_zero_velocity_note_ons_read_as_note_offs() {
        // On 60, then (running status) on 62, then both "off" as velocity 0.
        let track = [
            0x00, 0x91, 60, 100, 0x00, 62, 90, 0x60, 60, 0, 0x00, 62, 0, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let (read, end) = read_ticks(&smf(0, 96, &[&track]));
        assert_eq!(
            read,
            vec![
                (0, vec![0x91, 60, 100]),
                (0, vec![0x91, 62, 90]),
                (PPQN, vec![0x81, 60, 0]),
                (PPQN, vec![0x81, 62, 0]),
            ]
        );
        assert_eq!(end, PPQN);
    }

    #[test]
    fn ticks_are_rescaled_to_ppqn_rounding_to_the_nearest() {
        // 480 per beat: 240 is half a beat; 1 file tick is 2 app ticks.
        let track = [
            0x00, 0x90, 60, 100, 0x81, 0x70, 0x80, 60, 0, 0x01, 0x90, 61, 1, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let (read, _) = read_ticks(&smf(0, 480, &[&track]));
        let ticks: Vec<i32> = read.iter().map(|(tick, _)| *tick).collect();
        assert_eq!(ticks, vec![0, PPQN / 2, PPQN / 2 + 2]);

        // 7 per beat: tick 1 is 137.14… → 137.
        let track = [0x01, 0x90, 60, 100, 0x00, 0xFF, 0x2F, 0x00];
        assert_eq!(read_ticks(&smf(0, 7, &[&track])).0[0].0, 137);
    }

    #[test]
    fn every_track_is_merged_and_the_latest_end_wins() {
        // Type 1: a tempo-only conductor track, then two note tracks.
        let conductor = [
            0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let a = [
            0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let b = [
            0x30, 0x91, 64, 90, 0x81, 0x40, 0x81, 64, 0, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let (read, end) = read_ticks(&smf(1, 96, &[&conductor, &a, &b]));
        assert_eq!(
            read,
            vec![
                (0, vec![0x90, 60, 100]),
                (PPQN / 2, vec![0x91, 64, 90]),
                (PPQN, vec![0x80, 60, 0]),
                (PPQN * 5 / 2, vec![0x81, 64, 0]),
            ]
        );
        assert_eq!(end, PPQN * 5 / 2);
    }

    #[test]
    fn meta_sysex_and_unknown_chunks_are_skipped() {
        let track = [
            0x00, 0xFF, 0x03, 0x04, b'l', b'e', b'a', b'd', // track name
            0x00, 0xF0, 0x03, 0x7E, 0x09, 0xF7, // sysex
            0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0, // a note
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let mut file = smf(0, 96, &[]);
        file.extend_from_slice(b"XFIH");
        file.extend_from_slice(&3_u32.to_be_bytes());
        file.extend_from_slice(&[1, 2, 3]);
        file.extend_from_slice(b"MTrk");
        file.extend_from_slice(&(track.len() as u32).to_be_bytes());
        file.extend_from_slice(&track);

        let (read, _) = read_ticks(&file);
        assert_eq!(
            read,
            vec![(0, vec![0x90, 60, 100]), (PPQN, vec![0x80, 60, 0])]
        );
    }

    #[test]
    fn a_track_without_an_end_of_track_ends_at_its_last_event() {
        let track = [0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0];
        assert_eq!(read_ticks(&smf(0, 96, &[&track])).1, PPQN);
    }

    #[test]
    fn a_chunk_running_past_the_file_end_is_read_as_far_as_it_goes() {
        let mut file = smf(0, 96, &[&[0x00, 0x90, 60, 100, 0x60, 0x80, 60, 0]]);
        file[18..22].copy_from_slice(&1000_u32.to_be_bytes());
        assert_eq!(read_ticks(&file).0.len(), 2);
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        assert_eq!(read_smf(b"RIFF....").unwrap_err(), SmfError::NotMidi);
        assert_eq!(read_smf(b"MThd").unwrap_err(), SmfError::Malformed);
        // -25 fps, 40 sub-frames: the high bit of the division is set.
        assert_eq!(
            read_smf(&smf(0, 0xE728, &[])).unwrap_err(),
            SmfError::SmpteTiming
        );
        // A data byte with no running status to give it meaning.
        let orphan_data = [0x00, 60, 100];
        assert_eq!(
            read_smf(&smf(0, 96, &[&orphan_data])).unwrap_err(),
            SmfError::Malformed
        );
    }
}
