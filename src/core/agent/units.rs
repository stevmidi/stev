//! The units the agent bridge speaks (`360-mcp-server.md` decisions 8–9):
//! lengths in beats (quarter notes, whatever the meter) as decimals,
//! positions as `bar+beats` with bars from 1, pitches as note names with
//! C4 = MIDI 60, and notes as `pitch start length velocity` rows. Ticks never
//! leave this module. Pure, unit-tested below.

use crate::core::time::{Meter, PPQN, ticks_to_beats_f64};

/// The pitch-class names, sharps only (decision 8).
const PITCH_CLASSES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// How many decimals a beat count keeps: a 1/32 triplet still reads apart
/// from its neighbours, and the rows stay short.
const BEAT_DECIMALS: usize = 3;

/// `ticks` as beats: `1`, `0.5`, `-0.25`, `0.333`. Trailing zeros dropped.
pub(crate) fn format_beats(ticks: i32) -> String {
    let text = format!("{:.*}", BEAT_DECIMALS, ticks_to_beats_f64(ticks));
    // Always written with decimals, so trimming stops at the point at worst.
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A beat count (`4.5`, `-1`, `0.333`) as ticks, rounded to the nearest
/// tick, or `None` when `text` isn't a finite number.
pub(crate) fn parse_beats(text: &str) -> Option<i32> {
    let beats: f64 = text.trim().parse().ok()?;
    beats_to_ticks(beats)
}

/// `beats` as ticks, rounded, or `None` when not finite or out of range.
pub(crate) fn beats_to_ticks(beats: f64) -> Option<i32> {
    let ticks = (beats * f64::from(PPQN)).round();
    (ticks.is_finite() && ticks.abs() < f64::from(i32::MAX)).then_some(ticks as i32)
}

/// The project position `tick` as the ruler counts it: `9` on a bar line,
/// `9+3` three beats into bar 9, `9+2.5` between beats.
pub(crate) fn format_position(tick: i32, meter: Meter) -> String {
    let bar_ticks = meter.bar_ticks();
    let bar = tick.div_euclid(bar_ticks) + 1;
    match tick.rem_euclid(bar_ticks) {
        0 => bar.to_string(),
        offset => format!("{bar}+{}", format_beats(offset)),
    }
}

/// A position typed as [`format_position`] writes it (`9`, `9+3`,
/// `9 + 2.5`), or `None`. Bars count from 1; the beat offset may run past
/// the bar (`9+5` in 4/4 is `10+1`) but not below zero.
pub(crate) fn parse_position(text: &str, meter: Meter) -> Option<i32> {
    let (bar, offset) = match text.split_once('+') {
        Some((bar, beats)) => (bar, parse_beats(beats)?),
        None => (text, 0),
    };
    let bar: i32 = bar.trim().parse().ok()?;
    if bar < 1 || offset < 0 {
        return None;
    }
    meter.bar_ticks().checked_mul(bar - 1)?.checked_add(offset)
}

/// `pitch` as a note name, C4 = MIDI 60: `C4`, `F#2`, `C-1` for 0.
pub(crate) fn note_name(pitch: i32) -> String {
    format!("{}{}", pitch_class_name(pitch), pitch.div_euclid(12) - 1)
}

/// The pitch class of `pitch` as a name, sharps only: `A`, `C#`.
pub(crate) fn pitch_class_name(pitch: i32) -> &'static str {
    PITCH_CLASSES[pitch.rem_euclid(12) as usize]
}

/// A pitch as a note name (`C4`, `F#2`, `Bb3`, `C-1`, either case) or a
/// MIDI number (`60`). Flats are read though never written. Not clamped:
/// `C10` reads as 132, and the cleanup on a write clamps it.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the writing tools, `360` § Build phase 2")
)]
pub(crate) fn parse_pitch(text: &str) -> Option<i32> {
    let text = text.trim();
    if let Ok(number) = text.parse::<i32>() {
        return Some(number);
    }
    let mut chars = text.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let natural = PITCH_CLASSES
        .iter()
        .position(|&name| name == letter.to_string())? as i32;
    let rest = chars.as_str();
    let (accidental, octave) = match rest.chars().next()? {
        '#' => (1, &rest[1..]),
        'b' => (-1, &rest[1..]),
        _ => (0, rest),
    };
    let octave: i32 = octave.parse().ok()?;
    Some((octave + 1) * 12 + natural + accidental)
}

/// One note as a row (decision 9). Ticks, relative to whatever the rows
/// belong to (the clip being written, the range being read); `pitch` and
/// `velocity` as read, unclamped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoteRow {
    /// MIDI note number, C4 = 60.
    pub(crate) pitch: i32,
    /// Start, in ticks from the rows' origin; negative for a note already
    /// sounding there.
    pub(crate) start: i32,
    /// Length in ticks.
    pub(crate) length: i32,
    /// Note-on velocity.
    pub(crate) velocity: i32,
}

impl NoteRow {
    /// The row as text: `C4 0 1 90`.
    pub(crate) fn format(&self) -> String {
        format!(
            "{} {} {} {}",
            note_name(self.pitch),
            format_beats(self.start),
            format_beats(self.length),
            self.velocity
        )
    }

    /// One row, `pitch start length velocity` separated by whitespace or
    /// commas, or why it isn't one.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by the writing tools, `360` § Build phase 2")
    )]
    pub(crate) fn parse(line: &str) -> Result<NoteRow, String> {
        let fields: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|field| !field.is_empty())
            .collect();
        let [pitch, start, length, velocity] = fields[..] else {
            return Err("expected 4 fields: pitch start length velocity".to_owned());
        };
        Ok(NoteRow {
            pitch: parse_pitch(pitch).ok_or_else(|| format!("{pitch:?} isn't a pitch"))?,
            start: parse_beats(start).ok_or_else(|| format!("{start:?} isn't a start in beats"))?,
            length: parse_beats(length)
                .ok_or_else(|| format!("{length:?} isn't a length in beats"))?,
            velocity: velocity
                .parse()
                .map_err(|_| format!("{velocity:?} isn't a velocity"))?,
        })
    }
}

/// Every row in `text`, one per line (blank lines skipped), or the first
/// line that isn't one, by its 1-based number.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the writing tools, `360` § Build phase 2")
)]
pub(crate) fn parse_rows(text: &str) -> Result<Vec<NoteRow>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| NoteRow::parse(line).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meter(text: &str) -> Meter {
        Meter::parse(text).unwrap()
    }

    #[test]
    fn beats_read_as_short_decimals() {
        assert_eq!(format_beats(960), "1");
        assert_eq!(format_beats(480), "0.5");
        assert_eq!(format_beats(-240), "-0.25");
        assert_eq!(format_beats(320), "0.333");
        assert_eq!(format_beats(0), "0");
        assert_eq!(format_beats(-1), "-0.001");
        assert_eq!(format_beats(16 * 960), "16");
    }

    #[test]
    fn beats_parse_back_to_the_nearest_tick() {
        assert_eq!(parse_beats("4.5"), Some(4320));
        assert_eq!(parse_beats(" -1 "), Some(-960));
        assert_eq!(parse_beats("0.333"), Some(320));
        assert_eq!(parse_beats("x"), None);
        assert_eq!(parse_beats("inf"), None);
        for ticks in [0, 60, 120, 240, 320, 480, 960, 1440, -480] {
            assert_eq!(parse_beats(&format_beats(ticks)), Some(ticks), "{ticks}");
        }
    }

    #[test]
    fn positions_count_bars_from_one_in_every_meter() {
        let four = meter("4/4");
        assert_eq!(format_position(0, four), "1");
        assert_eq!(format_position(8 * 3840, four), "9");
        assert_eq!(format_position(8 * 3840 + 3 * 960, four), "9+3");
        assert_eq!(format_position(8 * 3840 + 2400, four), "9+2.5");

        // A bar of 3/4 and of 6/8 are both three quarters long.
        for text in ["3/4", "6/8"] {
            let m = meter(text);
            assert_eq!(format_position(2880, m), "2", "{text}");
            assert_eq!(format_position(2880 + 960, m), "2+1", "{text}");
        }
        // 7/8: three and a half beats to the bar.
        let seven = meter("7/8");
        assert_eq!(format_position(3360, seven), "2");
        assert_eq!(format_position(3360 + 3360 - 480, seven), "2+3");
    }

    #[test]
    fn positions_parse_back_in_every_meter() {
        for text in ["4/4", "3/4", "6/8", "7/8", "5/4"] {
            let m = meter(text);
            for tick in [0, 480, 960, m.bar_ticks(), 9 * m.bar_ticks() + 1440] {
                let written = format_position(tick, m);
                assert_eq!(parse_position(&written, m), Some(tick), "{text} {written}");
            }
        }
        let four = meter("4/4");
        assert_eq!(parse_position(" 9 + 3 ", four), Some(8 * 3840 + 2880));
        assert_eq!(parse_position("9+5", four), parse_position("10+1", four));
        assert_eq!(parse_position("0", four), None);
        assert_eq!(parse_position("9+-1", four), None);
        assert_eq!(parse_position("nine", four), None);
    }

    #[test]
    fn note_names_put_c4_at_60_with_sharps_only() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(note_name(45), "A2");
        assert_eq!(note_name(0), "C-1");
        assert_eq!(note_name(127), "G9");
        for pitch in 0..=127 {
            assert_eq!(parse_pitch(&note_name(pitch)), Some(pitch));
        }
        assert_eq!(parse_pitch("Db4"), Some(61));
        assert_eq!(parse_pitch("bb3"), Some(58));
        assert_eq!(parse_pitch("64"), Some(64));
        assert_eq!(parse_pitch("H4"), None);
        assert_eq!(parse_pitch("C"), None);
    }

    #[test]
    fn rows_round_trip() {
        let rows = vec![
            NoteRow {
                pitch: 60,
                start: 0,
                length: 960,
                velocity: 90,
            },
            NoteRow {
                pitch: 64,
                start: 960,
                length: 480,
                velocity: 78,
            },
            NoteRow {
                pitch: 67,
                start: -480,
                length: 1440,
                velocity: 80,
            },
        ];
        let text = rows
            .iter()
            .map(NoteRow::format)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, "C4 0 1 90\nE4 1 0.5 78\nG4 -0.5 1.5 80");
        assert_eq!(parse_rows(&text), Ok(rows));
    }

    #[test]
    fn rows_parse_leniently_and_name_the_bad_line() {
        let rows = parse_rows("  A2 0 1.5 96 \n\nC#3, 2, 0.25, 70\n60 3 1 100").unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].pitch, 49);
        assert_eq!(rows[1].start, 1920);
        assert_eq!(rows[2].pitch, 60);

        let error = parse_rows("C4 0 1 90\nC4 0 1").unwrap_err();
        assert!(error.starts_with("line 2:"), "{error}");
        let error = parse_rows("Q4 0 1 90").unwrap_err();
        assert!(error.contains("isn't a pitch"), "{error}");
    }
}
