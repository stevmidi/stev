//! The stress project: synthetic projects of any size, built in tests to
//! time what grows with a project (`420-large-projects.md`). The recovery
//! timing (`recovery_timing`) and the command sweep (`command_timing` in
//! `event_handlers`) build theirs here, and an ignored test writes the
//! design's target, [`STRESS_EVENTS`] events and [`STRESS_BLOB_BYTES`] of
//! plugin state, into the library to play by hand.
//!
//! Write it with `cargo test --release write_stress_project -- --ignored`.
//! It lands in the projects root's `Stress` folder. Its plugins point at a
//! bundle that doesn't exist, so none loads, but each track's state still
//! travels through every save, snapshot and recovery copy.

use crate::core::config::MAX_TRACKS;
use crate::core::sequencer::Sequencer;
use crate::core::sequencer::test_support::{bend_at, clip_at, param_at, test_sequencer};
use crate::core::xorshift::XorShift;
use crate::models::event::Event;
use crate::models::knobs::ParamId;
use crate::models::track::{InstrumentRef, TrackOutput};

use super::dto::ProjectData;
use super::storage::save_project;

/// Events in the design's target project, across all its clips
/// (`420-large-projects.md` decision 1).
pub(crate) const STRESS_EVENTS: usize = 100_000;

/// Bytes of plugin state in the design's target project, split evenly over
/// its tracks.
pub(crate) const STRESS_BLOB_BYTES: usize = 50 * MEGABYTE;

/// Bytes in a megabyte.
pub(crate) const MEGABYTE: usize = 1_000_000;

/// Clips on each track.
const CLIPS_PER_TRACK: usize = 5;

/// Ticks between one event and the next in a clip.
const EVENT_SPACING_TICKS: i32 = 24;

/// Events in one group of [`events`]: two notes and the wheel and knob
/// moves under them.
const GROUP: usize = 10;

/// Steps of a parameter move's value: enough that a value writes as many
/// digits as a real one.
const PARAM_VALUE_STEPS: u64 = 1 << 20;

/// The parameter the knob moves drive.
const PARAM: ParamId = 714;

/// The projects-root folder the stress project is written to.
const STRESS_FOLDER: &str = "Stress";

/// The stress project's file name.
const STRESS_NAME: &str = "Stress 100k";

/// `count` events of a clip, in groups of [`GROUP`]: two notes starting,
/// three bends, three parameter moves, the two notes ending. Four in ten are
/// notes' edges and the rest wheel and knob moves, as in a clip dense with
/// them: a parameter move is the dearest to write, with its float. A count
/// that isn't a whole number of groups leaves the last notes unended.
fn events(count: usize, rng: &mut XorShift) -> Vec<Event> {
    let mut pitches = [0u8; 2];
    (0..count)
        .map(|index| {
            let tick = index as i32 * EVENT_SPACING_TICKS;
            match index % GROUP {
                0 | 1 => {
                    let pitch = 36 + rng.below(48) as u8;
                    pitches[index % GROUP] = pitch;
                    Event::new(tick, 0, vec![0x90, pitch, 100])
                }
                2..=4 => bend_at(tick, rng.below(128) as u8),
                5..=7 => {
                    let value = rng.below(PARAM_VALUE_STEPS) as f64 / PARAM_VALUE_STEPS as f64;
                    param_at(tick, PARAM, value)
                }
                slot => Event::new(tick, 0, vec![0x80, pitches[slot - 8], 0]),
            }
        })
        .collect()
}

/// A plugin output holding `state_bytes` of state. What the bytes are makes
/// no difference to base64 or to the JSON around it.
fn instrument(state_bytes: usize) -> TrackOutput {
    TrackOutput::Instrument(InstrumentRef {
        bundle_path: "/Library/Audio/Plug-Ins/VST3/Stress.vst3".into(),
        plugin_id: "stress".to_owned(),
        display_name: "Stress".to_owned(),
        state: vec![0x5A; state_bytes],
    })
}

/// Fills `sequencer` with the maximum track count, [`CLIPS_PER_TRACK`]
/// back-to-back clips on each, `events` events and `blob_bytes` of plugin
/// state in total, split evenly over the tracks and clips.
pub(crate) fn fill_stress_project(
    sequencer: &mut Sequencer,
    events_total: usize,
    blob_bytes: usize,
) {
    sequencer.set_track_count(MAX_TRACKS);
    let mut rng = XorShift::new(1);
    let per_clip = events_total / (MAX_TRACKS * CLIPS_PER_TRACK);
    let clip_length = per_clip as i32 * EVENT_SPACING_TICKS;
    for track in sequencer.tracks_mut() {
        track.set_output(instrument(blob_bytes / MAX_TRACKS));
        for index in 0..CLIPS_PER_TRACK {
            let mut clip = clip_at(index as i32 * clip_length, clip_length);
            clip.restore_events(events(per_clip, &mut rng));
            clip.calculate_note_lengths();
            track.add_clip(&clip);
        }
    }
}

/// Writes the design's target project into the library. See the module
/// docs.
#[test]
#[ignore = "writes into the user's library: run it by hand"]
fn write_stress_project() {
    let mut sequencer = test_sequencer();
    fill_stress_project(&mut sequencer, STRESS_EVENTS, STRESS_BLOB_BYTES);
    save_project(
        &ProjectData::from_sequencer(&sequencer),
        Some(STRESS_FOLDER),
        STRESS_NAME,
    )
    .unwrap();
}
