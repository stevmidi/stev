//! Capture fixtures: real takes kept as test data for the phrase-start
//! detection (`220-capture-without-pending-view.md`).
//!
//! Debug builds only. With `STEV_CAPTURE_DIR` set, every stopped `/`
//! writes the capture it framed (the trimmed buffer, in its own capture
//! ticks), the loop length the detection saw, and the start it picked to a
//! JSON file in that directory. `picked_start` starts out as the detected
//! start; once the user has corrected the start with `[` and saved the
//! project, the ignored `extract_picked_starts` test reads the corrected
//! window back from the `.stev` and writes it into the fixture. Checked-in
//! fixtures live in `fixtures/captures/`, and
//! `the_detection_picks_every_fixture_start` pins the detection to them.

use std::{env, fs, io, path::PathBuf};

use chrono::Local;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use std::path::Path;

use crate::core::time::Meter;
use crate::models::{clip::Clip, event::EventType};

#[cfg(test)]
use crate::models::event::Event;

/// The environment variable naming the directory captures are dumped to.
const CAPTURE_DIR_VAR: &str = "STEV_CAPTURE_DIR";

/// One note of a take: `[tick, length, pitch, velocity]` on disk.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FixtureNote(pub(crate) i32, pub(crate) i32, pub(crate) u8, pub(crate) u8);

/// A recorded take and the clip start the user wants from it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct CaptureFixture {
    /// What the take is, in words — filled in by hand.
    #[serde(default)]
    pub(crate) description: String,
    /// Tempo at capture time, µs per quarter note.
    pub(crate) tempo_us: i32,
    /// The project's meter at capture time, `[numerator, denominator]`.
    /// Absent in fixtures recorded before time signatures: 4/4.
    #[serde(default = "four_four")]
    pub(crate) meter: [u8; 2],
    /// `Sequencer::loop_reference_length` at capture time — the detection
    /// window's raw length.
    pub(crate) loop_reference_length: i32,
    /// The start the detection picked when the take was committed.
    pub(crate) detected_start: i32,
    /// The start the user wants: the detected one until corrected.
    pub(crate) picked_start: i32,
    /// The take's notes, in capture ticks.
    pub(crate) notes: Vec<FixtureNote>,
}

/// The meter of a fixture recorded before time signatures.
fn four_four() -> [u8; 2] {
    [4, 4]
}

impl CaptureFixture {
    /// Snapshots `capture` (sorted, lengths calculated, trimmed to the
    /// buffer) with the start the detection picked from it.
    pub(crate) fn from_capture(
        capture: &Clip,
        tempo_us: i32,
        meter: Meter,
        loop_reference_length: i32,
        detected_start: i32,
    ) -> Self {
        let notes = capture
            .events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| {
                FixtureNote(
                    event.tick(),
                    event.end_tick() - event.tick(),
                    event.note_number().unwrap_or(0),
                    event.velocity().unwrap_or(0),
                )
            })
            .collect();
        Self {
            description: String::new(),
            tempo_us,
            meter: [meter.numerator(), meter.denominator()],
            loop_reference_length,
            detected_start,
            picked_start: detected_start,
            notes,
        }
    }

    /// Writes the fixture to `$STEV_CAPTURE_DIR/capture-<timestamp>.json`
    /// when that variable is set; does nothing otherwise. Failures are only
    /// logged: a dump must never get in the way of the commit.
    pub(crate) fn write_if_requested(&self) {
        let Some(dir) = env::var_os(CAPTURE_DIR_VAR).map(PathBuf::from) else {
            return;
        };
        let name = Local::now().format("capture-%Y%m%d-%H%M%S.json");
        let path = dir.join(name.to_string());
        let written = fs::create_dir_all(&dir)
            .and_then(|()| self.to_json())
            .and_then(|json| fs::write(&path, json));
        match written {
            Ok(()) => dprintln!("capture fixture: {}", path.display()),
            Err(err) => dprintln!("capture fixture: {} not written: {err}", path.display()),
        }
    }

    /// Pretty JSON with each note on one line, so a fixture reads (and can be
    /// edited) as a note list.
    pub(crate) fn to_json(&self) -> io::Result<String> {
        let pretty = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let mut out = String::with_capacity(pretty.len());
        let mut note: Option<String> = None;
        for line in pretty.lines() {
            let trimmed = line.trim();
            match note.as_mut() {
                None if trimmed == "[" && line.len() > trimmed.len() + 2 => {
                    note = Some(format!("{}[", &line[..line.len() - 1]));
                }
                None => {
                    out.push_str(line);
                    out.push('\n');
                }
                Some(open) if trimmed.starts_with(']') => {
                    out.push_str(open.trim_end_matches(", "));
                    out.push_str(trimmed);
                    out.push('\n');
                    note = None;
                }
                Some(open) => {
                    open.push_str(trimmed.trim_end_matches(','));
                    open.push_str(", ");
                }
            }
        }
        Ok(out)
    }

    /// The meter the take was played in; an unsupported one reads as 4/4.
    #[cfg(test)]
    pub(crate) fn meter(&self) -> Meter {
        Meter::new(self.meter[0], self.meter[1]).unwrap_or(Meter::FOUR_FOUR)
    }

    /// The take as a capture clip: note pairs, sorted, lengths calculated —
    /// what the detection is handed.
    #[cfg(test)]
    pub(crate) fn to_clip(&self) -> Clip {
        let mut clip = Clip::new();
        for &FixtureNote(tick, length, pitch, velocity) in &self.notes {
            clip.add_event(Event::new(tick, 0, vec![0x90, pitch, velocity]));
            clip.add_event(Event::new(tick + length, 0, vec![0x80, pitch, 0]));
        }
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip
    }

    /// Every `*.json` fixture in `dir`, sorted by file name.
    #[cfg(test)]
    pub(crate) fn load_dir(dir: &Path) -> Vec<(PathBuf, Self)> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort_unstable();
        paths
            .into_iter()
            .map(|path| {
                let json = fs::read_to_string(&path).expect("fixture readable");
                let fixture = serde_json::from_str(&json)
                    .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
                (path, fixture)
            })
            .collect()
    }

    /// The checked-in fixtures directory.
    #[cfg(test)]
    pub(crate) fn checked_in_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/captures")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        project::{ClipData, EventData, ProjectData},
        sequencer::Sequencer,
    };

    /// The detection starts every recorded take where the user wants it:
    /// real takes (2026-09-26), whose starts the user accepted or corrected
    /// with `[`. A change to the phrase-start detection that moves one of
    /// them is a regression, unless the user picks the new start.
    #[test]
    fn the_detection_picks_every_fixture_start() {
        let fixtures = CaptureFixture::load_dir(&CaptureFixture::checked_in_dir());
        assert!(!fixtures.is_empty(), "fixtures/captures/ is empty");
        for (path, fixture) in fixtures {
            let (start, _) = Sequencer::detected_phrase_window(
                &fixture.to_clip(),
                fixture.loop_reference_length,
                fixture.meter(),
            );
            assert_eq!(start, fixture.picked_start, "{}", path.display());
        }
    }

    /// `NoteOn` `(tick, pitch)` pairs of a saved clip, sorted by tick.
    fn saved_note_ons(clip: &ClipData) -> Vec<(i32, u8)> {
        let mut ons: Vec<(i32, u8)> = clip
            .events
            .iter()
            .filter(|event| event.midi_message.first().is_some_and(|b| b & 0xF0 == 0x90))
            .filter(|event| event.midi_message.get(2).is_some_and(|&v| v > 0))
            .map(|event| (event.tick, event.midi_message[1]))
            .collect();
        ons.sort_unstable();
        ons
    }

    /// Where `clip`'s window starts in `fixture`'s capture ticks, if `clip`
    /// is that take: same pitches in the same order, its ticks an affine map
    /// of the fixture's (a rebase shift, and Enter's tempo-fit scale).
    fn picked_start_in(fixture: &CaptureFixture, clip: &ClipData) -> Option<i32> {
        let mut ours: Vec<(i32, u8)> = fixture
            .notes
            .iter()
            .map(|&FixtureNote(tick, _, pitch, _)| (tick, pitch))
            .collect();
        ours.sort_unstable();
        let theirs = saved_note_ons(clip);
        if ours.len() < 2
            || ours.len() != theirs.len()
            || ours.iter().zip(&theirs).any(|(a, b)| a.1 != b.1)
        {
            return None;
        }

        let (first, last) = (ours[0].0, ours[ours.len() - 1].0);
        let (saved_first, saved_last) = (theirs[0].0, theirs[theirs.len() - 1].0);
        if last == first {
            return None;
        }
        let scale = f64::from(saved_last - saved_first) / f64::from(last - first);
        let raw = f64::from(clip.region_start - saved_first) / scale + f64::from(first);
        Some(raw.round() as i32)
    }

    /// Dev tool, not a test: reads the saved project `$STEV_PROJECT` and
    /// writes each take's corrected start into its fixture in
    /// `$STEV_CAPTURE_DIR`. Run with
    /// `cargo test extract_picked_starts -- --ignored --nocapture`.
    #[test]
    #[ignore = "dev tool: needs STEV_CAPTURE_DIR and STEV_PROJECT"]
    fn extract_picked_starts() {
        let dir = PathBuf::from(env::var_os(CAPTURE_DIR_VAR).expect("STEV_CAPTURE_DIR"));
        let project_path = PathBuf::from(env::var_os("STEV_PROJECT").expect("STEV_PROJECT"));
        let json = fs::read_to_string(&project_path).expect("project readable");
        let project: ProjectData = serde_json::from_str(&json).expect("project parses");
        let clips: Vec<&ClipData> = project.tracks.iter().flat_map(|t| &t.clips).collect();

        for (path, mut fixture) in CaptureFixture::load_dir(&dir) {
            let Some(picked) = clips
                .iter()
                .find_map(|clip| picked_start_in(&fixture, clip))
            else {
                dprintln!("{}: no matching clip", path.display());
                continue;
            };
            dprintln!(
                "{}: detected {} picked {} ({:+} ticks)",
                path.display(),
                fixture.detected_start,
                picked,
                picked - fixture.detected_start
            );
            fixture.picked_start = picked;
            let json = fixture.to_json().expect("fixture serializes");
            fs::write(&path, json).expect("fixture writable");
        }
    }

    #[test]
    fn a_fixture_round_trips_through_its_clip() {
        let fixture = CaptureFixture {
            description: String::new(),
            tempo_us: 500_000,
            meter: [4, 4],
            loop_reference_length: 7680,
            detected_start: 960,
            picked_start: 960,
            notes: vec![FixtureNote(0, 400, 60, 100), FixtureNote(960, 200, 62, 90)],
        };
        let clip = fixture.to_clip();
        let again = CaptureFixture::from_capture(&clip, 500_000, Meter::FOUR_FOUR, 7680, 960);
        assert_eq!(again.notes, fixture.notes);

        let json = fixture.to_json().unwrap();
        assert!(
            json.contains("\n    [0, 400, 60, 100],\n"),
            "one note per line:\n{json}"
        );
        let parsed: CaptureFixture = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.notes, fixture.notes);
    }

    #[test]
    fn a_saved_clip_is_matched_through_a_shift_and_a_tempo_scale() {
        let fixture = CaptureFixture {
            description: String::new(),
            tempo_us: 500_000,
            meter: [4, 4],
            loop_reference_length: 7680,
            detected_start: 1000,
            picked_start: 1000,
            notes: vec![
                FixtureNote(100, 50, 60, 100),
                FixtureNote(1000, 50, 62, 100),
                FixtureNote(2100, 50, 64, 100),
            ],
        };
        // Shifted by +500, then scaled ×2 about 0: the user moved the start
        // to raw tick 1500.
        let map = |tick: i32| (tick + 500) * 2;
        let clip = ClipData {
            start_tick: 0,
            region_start: map(1500),
            region_end: map(2200),
            muted: false,
            swing_pct: 50,
            events: fixture
                .notes
                .iter()
                .map(|&FixtureNote(tick, length, pitch, velocity)| EventData {
                    tick: map(tick),
                    length,
                    midi_message: vec![0x90, pitch, velocity],
                    muted: false,
                })
                .collect(),
        };
        assert_eq!(picked_start_in(&fixture, &clip), Some(1500));

        let mut other = fixture.clone();
        other.notes[1].2 = 65;
        assert_eq!(picked_start_in(&other, &clip), None, "different take");
    }
}
