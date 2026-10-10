//! Serializable project data structures and the mapping to/from the runtime
//! `Sequencer`. Persistence serializes these explicit DTOs, never runtime
//! structs or UI state directly (see `060-persistence.md`).
//!
//! **These field names are the literal JSON keys in every saved `.stev`** —
//! there is no `#[serde(rename)]` indirection. Renaming a required field breaks
//! every existing project on load (`080-conventions.md`); a new field must be
//! `#[serde(default)]` with a neutral default so older projects still parse.
//! The `tick`/`ticks` naming rule applies here too, but changing a name is a
//! file-format change, not a rename.

use std::hash::{DefaultHasher, Hasher};
use std::io::{self, Write};
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::{
    core::{config, sequencer::Sequencer, time::Meter},
    metadata::clip_metadata::ClipMetadata,
    models::{
        clip::Clip,
        event::Event,
        track::{InstrumentRef, TrackOutput, track_name_from_input},
    },
};

/// One MIDI event, on disk. `tick` is an event-tick position; `length` an
/// amount (ticks to the paired `NoteOff`).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct EventData {
    /// Event-tick position within the clip.
    pub(crate) tick: i32,
    /// Note length in ticks (`NoteOn` only; `0` otherwise).
    pub(crate) length: i32,
    /// Raw MIDI bytes.
    pub(crate) midi_message: Vec<u8>,
    /// Whether the event is muted. Absent in projects saved before per-event
    /// mute existed — `#[serde(default)]` gives them the unmuted default.
    #[serde(default)]
    pub(crate) muted: bool,
}

/// One clip, on disk.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ClipData {
    /// Clip start on the arrangement timeline.
    pub(crate) start_tick: i32,
    /// Loop-window start, in event ticks.
    pub(crate) region_start: i32,
    /// Loop-window end, in event ticks.
    pub(crate) region_end: i32,
    /// Whether the clip is muted.
    pub(crate) muted: bool,
    /// Swing amount 50–75. Absent in projects from before swing detection —
    /// [`default_swing_pct`] gives them straight timing.
    #[serde(default = "default_swing_pct")]
    pub(crate) swing_pct: u8,
    /// The clip's events.
    #[serde(default)]
    pub(crate) events: Vec<EventData>,
}

/// Serde default for [`ClipData::swing_pct`] — straight (50).
fn default_swing_pct() -> u8 {
    50
}

/// A track's output routing, on disk. `#[serde(tag = "type")]` — the JSON has a
/// `"type"` discriminant.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
pub(crate) enum TrackOutputData {
    /// External MIDI output on a channel.
    MidiOut {
        /// MIDI channel, 0–15.
        channel: u8,
    },
    /// A hosted instrument plugin (CLAP or VST3).
    Instrument {
        /// Empty when deserialized from a phase-3 project (tag only) or a
        /// project saved before a plugin was picked — treated as "no plugin".
        #[serde(default)]
        bundle_path: String,
        /// Plugin id within the bundle.
        #[serde(default)]
        plugin_id: String,
        /// Cached display name.
        #[serde(default)]
        display_name: String,
        /// Base64 of the plugin's opaque state blob (active preset). Empty /
        /// absent for a project saved before plugin state was persisted, or a
        /// plugin that saves none. macOS plugin host — see `130-plugin-host.md`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        state: String,
    },
}

impl TrackOutputData {
    /// The runtime routing this loads as.
    fn into_output(self) -> TrackOutput {
        match self {
            TrackOutputData::MidiOut { channel } => TrackOutput::MidiOut { channel },
            TrackOutputData::Instrument {
                bundle_path,
                plugin_id,
                display_name,
                state,
            } if !bundle_path.is_empty() => TrackOutput::Instrument(InstrumentRef {
                bundle_path: PathBuf::from(bundle_path),
                plugin_id,
                display_name,
                // A corrupt blob just means "load the plugin at its
                // default" — never fail the whole project load for it.
                state: BASE64.decode(state).unwrap_or_default(),
            }),
            // Instrument track with no plugin reference (phase-3 project
            // or saved before a plugin was picked) — fall back to MIDI.
            TrackOutputData::Instrument { .. } => TrackOutput::MidiOut { channel: 0 },
        }
    }
}

/// One track, on disk.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct TrackData {
    /// Output routing.
    pub(crate) output: TrackOutputData,
    /// Mixer volume in dB (`0.0` = unity) and stereo balance (`-1.0`..=`1.0`,
    /// `0.0` = centre). `#[serde(default)]` alone suffices — `0.0` *is* the
    /// neutral value, so a project saved before this field existed loads at
    /// unity/centre. See `060-persistence.md`.
    #[serde(default)]
    pub(crate) volume_db: f32,
    /// See [`volume_db`](Self::volume_db).
    #[serde(default)]
    pub(crate) pan: f32,
    /// Track mute. `#[serde(default)]` (`false`) alone suffices — unmuted *is*
    /// the neutral value, so a project saved before this field existed loads
    /// audible. Solo is deliberately **not** persisted (transient monitoring
    /// state) and is cleared on load. See `060-persistence.md`.
    #[serde(default)]
    pub(crate) muted: bool,
    /// Which of the theme's track colours the track draws in
    /// (`Track::color_slot`). Absent in a project saved before tracks kept
    /// their colour — it then takes its position's, so it looks as it did.
    #[serde(default)]
    pub(crate) color: Option<usize>,
    /// The user's name for the track (`Track::name`); absent while unnamed
    /// (the header shows the track's number) and in a project saved before
    /// tracks had names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// The track's clips.
    pub(crate) clips: Vec<ClipData>,
}

/// An [`io::Write`] that feeds everything written into a hasher — what
/// [`ProjectData::fingerprint`] serializes into.
struct HashWriter(DefaultHasher);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One time-signature entry, on disk: `numerator` / `denominator` from the
/// zero-based bar `bar_index` on. A project holds a list of these so meter
/// changes along the timeline could come later without a format change, but
/// today it is exactly one entry at bar 0 and loading reads only the first
/// (`archive/270-time-signature.md`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct MeterData {
    /// Zero-based bar the meter starts on.
    pub(crate) bar_index: i32,
    /// Counted beats per bar.
    pub(crate) numerator: u8,
    /// The counted beat's note value.
    pub(crate) denominator: u8,
}

impl MeterData {
    /// The one-entry list a project stores for `meter`.
    fn list_for(meter: Meter) -> Vec<MeterData> {
        vec![MeterData {
            bar_index: 0,
            numerator: meter.numerator(),
            denominator: meter.denominator(),
        }]
    }
}

/// The `meter` a project saved before time signatures existed: 4/4.
fn default_meter_list() -> Vec<MeterData> {
    MeterData::list_for(Meter::FOUR_FOUR)
}

/// The project meter a saved list means: its first entry, or 4/4 when the
/// list is empty or that entry isn't a supported meter (a hand-edited file).
fn meter_from_list(list: &[MeterData]) -> Meter {
    let Some(first) = list.first() else {
        return Meter::FOUR_FOUR;
    };
    Meter::new(first.numerator, first.denominator).unwrap_or_else(|| {
        dprintln!(
            "Unsupported meter {}/{} in project, using 4/4",
            first.numerator,
            first.denominator
        );
        Meter::FOUR_FOUR
    })
}

/// A whole project, on disk — the root of the `.stev` JSON.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ProjectData {
    /// Tempo in microseconds per quarter note.
    pub(crate) tempo_us: i32,
    /// The time signature, as a list of changes ([`MeterData`]). Absent in
    /// projects saved before time signatures, which load as 4/4.
    #[serde(default = "default_meter_list")]
    pub(crate) meter: Vec<MeterData>,
    /// Loop-region start tick.
    pub(crate) region_start: i32,
    /// Loop-region end tick.
    pub(crate) region_end: i32,
    /// The tracks, `1..=MAX_TRACKS` of them (`apply_to_sequencer` clamps).
    pub(crate) tracks: Vec<TrackData>,
}

impl Default for ProjectData {
    fn default() -> Self {
        ProjectData {
            tempo_us: config::TEMPO_US_DEFAULT,
            meter: default_meter_list(),
            region_start: 0,
            region_end: config::REGION_LENGTH_DEFAULT,
            // One empty MIDI-Out track per slot, channel = track index —
            // mirrors `Sequencer::new`'s own defaults. Without an explicit
            // entry per track, `apply_to_sequencer`'s loop (bounded by
            // `self.tracks.len()`) would never call `track.set_output`, so a
            // track routed to an instrument plugin in the previous project would
            // survive `⌘N` untouched (`new_project` only clears clips).
            tracks: (0..config::DEFAULT_TRACK_COUNT)
                .map(|i| TrackData {
                    output: TrackOutputData::MidiOut { channel: i as u8 },
                    volume_db: 0.0,
                    pan: 0.0,
                    muted: false,
                    color: Some(i),
                    name: None,
                    clips: Vec::new(),
                })
                .collect(),
        }
    }
}

impl ProjectData {
    /// A hash of everything this project would write to disk but the
    /// plugins' state blobs: two snapshots with the same fingerprint save the
    /// same file, presets aside. The unsaved-changes check compares the live
    /// project's against the last save or load's
    /// (`EventHandlers::project_action_workflow`, `060-persistence.md`).
    /// The blobs are left out because plenty of plugins write a different one
    /// on every save of an unchanged preset (Nexus encrypts it afresh, DUNE
    /// recompresses it), so they would make such a project always look
    /// changed. Takes the snapshot by value — every caller is done with it —
    /// and streams the JSON into the hasher, so nothing is copied or buffered.
    /// Stable within one run only — never persisted.
    pub(crate) fn fingerprint(mut self) -> u64 {
        for track in &mut self.tracks {
            if let TrackOutputData::Instrument { state, .. } = &mut track.output {
                state.clear();
            }
        }
        let mut hasher = HashWriter(DefaultHasher::new());
        // Writing into a hasher can't fail; a serialization error would hash
        // a prefix, which still differs from any complete project.
        serde_json::to_writer(&mut hasher, &self).ok();
        hasher.0.finish()
    }

    /// Snapshots the sequencer into a serializable `ProjectData` (base64-encodes
    /// each plugin's state blob).
    pub(crate) fn from_sequencer(sequencer: &Sequencer) -> Self {
        let tracks = sequencer
            .tracks()
            .iter()
            .enumerate()
            .map(|(track_idx, track)| {
                let clips = track
                    .clips()
                    .iter()
                    .map(|clip| {
                        let events = clip
                            .events()
                            .iter()
                            .map(|event| EventData {
                                tick: event.tick(),
                                length: event.end_tick() - event.tick(),
                                midi_message: event.midi_message().to_vec(),
                                muted: event.is_muted(),
                            })
                            .collect::<Vec<_>>();

                        ClipData {
                            start_tick: clip.start_tick(),
                            region_start: clip.region().start(),
                            region_end: clip.region().end(),
                            muted: clip.is_muted(),
                            swing_pct: clip.swing_pct(),
                            events,
                        }
                    })
                    .collect::<Vec<_>>();

                TrackData {
                    output: match track.output() {
                        TrackOutput::MidiOut { channel } => {
                            TrackOutputData::MidiOut { channel: *channel }
                        }
                        TrackOutput::Instrument(inst) => TrackOutputData::Instrument {
                            bundle_path: inst.bundle_path.to_string_lossy().into_owned(),
                            plugin_id: inst.plugin_id.clone(),
                            display_name: inst.display_name.clone(),
                            state: BASE64.encode(&inst.state),
                        },
                    },
                    volume_db: sequencer.track_volume_db(track_idx),
                    pan: sequencer.track_pan(track_idx),
                    muted: sequencer.track_muted(track_idx),
                    color: Some(track.color_slot()),
                    name: track.name().map(str::to_owned),
                    clips,
                }
            })
            .collect();

        ProjectData {
            tempo_us: sequencer.tempo_us(),
            meter: MeterData::list_for(sequencer.meter()),
            region_start: sequencer.region_start(),
            region_end: sequencer.region_end(),
            tracks,
        }
    }

    /// The plugin each track will load, by engine slot, as
    /// [`apply_to_sequencer`](Self::apply_to_sequencer) sets the tracks up
    /// (slot = position, the first `MAX_TRACKS` tracks) — what a project load
    /// stages before it is applied (`130-plugin-host.md`).
    pub(crate) fn instrument_specs(&self) -> Vec<(usize, InstrumentRef)> {
        self.tracks
            .iter()
            .take(config::MAX_TRACKS)
            .enumerate()
            .filter_map(|(slot, track)| match track.output.clone().into_output() {
                TrackOutput::Instrument(instrument) => Some((slot, instrument)),
                TrackOutput::MidiOut { .. } => None,
            })
            .collect()
    }

    /// Replaces the whole session with this project (starts from
    /// `new_project`, decodes plugin state blobs, rebuilds clips). Returns the
    /// clip metadata for the UI. Consumes the data so event bytes move into
    /// the clips rather than being copied.
    pub(crate) fn apply_to_sequencer(self, sequencer: &mut Sequencer) -> Vec<ClipMetadata> {
        sequencer.new_project();
        sequencer.set_track_count(self.tracks.len());

        sequencer.set_tempo(self.tempo_us);
        sequencer.set_meter(meter_from_list(&self.meter));
        sequencer.set_global_region(self.region_start, self.region_end);

        let mut all_metadata = Vec::new();

        for (track_idx, track_data) in self.tracks.into_iter().enumerate() {
            sequencer.set_track_volume(track_idx, track_data.volume_db);
            sequencer.set_track_pan(track_idx, track_data.pan);
            sequencer.set_track_muted(track_idx, track_data.muted);
            if let Some(track) = sequencer.tracks_mut().get_mut(track_idx) {
                track.set_color_slot(track_data.color.unwrap_or(track_idx));
                // Through the rename's own rules, so a hand-edited blank name
                // shows the number.
                track.set_name(track_data.name.as_deref().and_then(track_name_from_input));
                track.set_output(track_data.output.into_output());

                for clip_data in track_data.clips {
                    let mut clip = Clip::new();
                    clip.set_start_tick(clip_data.start_tick);
                    clip.region_mut()
                        .set_region(Some(clip_data.region_start), Some(clip_data.region_end));
                    clip.set_muted(clip_data.muted);
                    clip.set_swing_pct(clip_data.swing_pct);

                    // Add events (delta_ticks recalculated by add_event)
                    for event_data in clip_data.events {
                        let mut event =
                            Event::new(event_data.tick, event_data.length, event_data.midi_message);
                        event.set_muted(event_data.muted);
                        clip.add_event(event);
                    }

                    track.add_clip(&clip);
                    all_metadata.push(ClipMetadata::from_clip(track_idx, &clip));
                }
            }
        }

        all_metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequencer::test_support::{test_sequencer, track_colors};

    /// A default project with `count` tracks.
    fn project_with_tracks(count: usize) -> ProjectData {
        let mut data = ProjectData::default();
        let track = data.tracks[0].clone();
        data.tracks = vec![track; count];
        data
    }

    /// The same project fingerprints the same; any saved field changing —
    /// here the tempo, a track's name — changes it.
    #[test]
    fn a_fingerprint_changes_with_any_saved_field() {
        let saved = ProjectData::default().fingerprint();
        assert_eq!(ProjectData::default().fingerprint(), saved);

        let mut data = ProjectData::default();
        data.tempo_us += 1;
        assert_ne!(data.fingerprint(), saved);

        let mut data = ProjectData::default();
        data.tracks[1].name = Some("Bass".to_owned());
        assert_ne!(data.fingerprint(), saved);
    }

    /// A plugin's state blob is left out: a plugin that writes a different
    /// blob for the same preset on every save must not make the project look
    /// changed. Which plugin a track holds still counts.
    #[test]
    fn a_fingerprint_ignores_plugin_state_but_not_the_plugin() {
        let with_plugin = |plugin_id: &str, state: &str| {
            let mut data = ProjectData::default();
            data.tracks[0].output = TrackOutputData::Instrument {
                bundle_path: "/Synth.vst3".to_owned(),
                plugin_id: plugin_id.to_owned(),
                display_name: "Synth".to_owned(),
                state: state.to_owned(),
            };
            data.fingerprint()
        };
        assert_eq!(with_plugin("a", "AAAA"), with_plugin("a", "BBBB"));
        assert_ne!(with_plugin("a", "AAAA"), with_plugin("b", "AAAA"));
    }

    /// What a load stages is what the applied project then asks for
    /// (`ProjectLoaded`'s `instruments`), past the track cap too.
    #[test]
    fn the_staged_plugins_match_the_applied_tracks() {
        let mut data = project_with_tracks(config::MAX_TRACKS + 2);
        for idx in [1, 3, config::MAX_TRACKS - 1, config::MAX_TRACKS + 1] {
            data.tracks[idx].output = TrackOutputData::Instrument {
                bundle_path: format!("/Synth{idx}.vst3"),
                plugin_id: "synth".to_owned(),
                display_name: "Synth".to_owned(),
                state: BASE64.encode([idx as u8]),
            };
        }
        // A plugin track with no plugin named loads as MIDI.
        data.tracks[5].output = TrackOutputData::Instrument {
            bundle_path: String::new(),
            plugin_id: String::new(),
            display_name: String::new(),
            state: String::new(),
        };
        let staged = data.instrument_specs();
        assert_eq!(
            staged.iter().map(|(slot, _)| *slot).collect::<Vec<_>>(),
            [1, 3, config::MAX_TRACKS - 1]
        );

        let mut sequencer = test_sequencer();
        data.apply_to_sequencer(&mut sequencer);
        let applied: Vec<_> = sequencer
            .tracks()
            .iter()
            .filter_map(|track| match track.output() {
                TrackOutput::Instrument(r) => Some((track.slot(), r.clone())),
                TrackOutput::MidiOut { .. } => None,
            })
            .collect();
        assert_eq!(staged, applied);
    }

    #[test]
    fn a_loaded_project_keeps_its_own_track_count() {
        let mut sequencer = test_sequencer();

        project_with_tracks(8).apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), 8);

        ProjectData::default().apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), config::DEFAULT_TRACK_COUNT);
    }

    #[test]
    fn a_loaded_track_count_is_clamped_to_one_through_max() {
        let mut sequencer = test_sequencer();

        project_with_tracks(config::MAX_TRACKS + 3).apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), config::MAX_TRACKS);

        project_with_tracks(0).apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.tracks().len(), 1);
    }

    #[test]
    fn track_names_survive_a_save_and_load() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].set_name(Some("Bass".to_owned()));
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();

        sequencer.new_project();
        assert_eq!(sequencer.tracks()[1].name(), None, "a new project unnames");
        let data: ProjectData = serde_json::from_str(&json).unwrap();
        data.apply_to_sequencer(&mut sequencer);

        let names: Vec<Option<&str>> = sequencer.tracks().iter().map(|t| t.name()).collect();
        assert_eq!(names, vec![None, Some("Bass"), None, None]);
    }

    #[test]
    fn a_meter_survives_a_save_and_load() {
        let seven_eight = Meter::new(7, 8).unwrap();
        let mut sequencer = test_sequencer();
        sequencer.set_meter(seven_eight);
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();

        sequencer.new_project();
        assert_eq!(sequencer.meter(), Meter::FOUR_FOUR, "a new project is 4/4");
        let data: ProjectData = serde_json::from_str(&json).unwrap();
        assert_eq!(
            data.meter,
            MeterData::list_for(seven_eight),
            "one entry at bar 0"
        );
        data.apply_to_sequencer(&mut sequencer);

        assert_eq!(sequencer.meter(), seven_eight);
    }

    /// A project saved before time signatures has no `meter` key: it loads
    /// as 4/4, even over a session in another meter.
    #[test]
    fn a_project_without_a_meter_loads_as_four_four() {
        let mut json = serde_json::to_value(ProjectData::default()).unwrap();
        json.as_object_mut().unwrap().remove("meter");
        let data: ProjectData = serde_json::from_value(json).unwrap();
        assert_eq!(data.meter, default_meter_list());

        let mut sequencer = test_sequencer();
        sequencer.set_meter(Meter::new(3, 4).unwrap());
        data.apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.meter(), Meter::FOUR_FOUR);
    }

    /// A hand-edited meter Stev can't play, or an empty list, falls back to
    /// 4/4 instead of failing the load; only the first entry is read.
    #[test]
    fn a_meter_list_reads_its_first_supported_entry_or_four_four() {
        let entry = |numerator, denominator| MeterData {
            bar_index: 0,
            numerator,
            denominator,
        };
        assert_eq!(meter_from_list(&[]), Meter::FOUR_FOUR);
        assert_eq!(meter_from_list(&[entry(5, 16)]), Meter::FOUR_FOUR);
        assert_eq!(meter_from_list(&[entry(0, 4)]), Meter::FOUR_FOUR);
        assert_eq!(
            meter_from_list(&[entry(6, 8), entry(4, 4)]),
            Meter::new(6, 8).unwrap()
        );
    }

    #[test]
    fn a_fingerprint_changes_with_the_meter() {
        let data = ProjectData {
            meter: MeterData::list_for(Meter::new(3, 4).unwrap()),
            ..ProjectData::default()
        };
        assert_ne!(data.fingerprint(), ProjectData::default().fingerprint());
    }

    #[test]
    fn an_unnamed_track_saves_no_name_and_a_blank_one_loads_unnamed() {
        let sequencer = test_sequencer();
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();
        assert!(!json.contains("\"name\""));

        let mut data = project_with_tracks(2);
        data.tracks[0].name = Some("  ".to_owned());
        data.tracks[1].name = Some(" Keys ".to_owned());
        let mut sequencer = test_sequencer();
        data.apply_to_sequencer(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].name(), None);
        assert_eq!(sequencer.tracks()[1].name(), Some("Keys"));
    }

    #[test]
    fn track_colours_survive_a_save_and_load() {
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[1].set_color_slot(9);
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();

        // Even after slots and colours were reset by a new project.
        sequencer.new_project();
        let data: ProjectData = serde_json::from_str(&json).unwrap();
        data.apply_to_sequencer(&mut sequencer);

        assert_eq!(track_colors(&sequencer), vec![0, 9, 2, 3]);
    }

    #[test]
    fn a_track_saved_without_a_colour_takes_its_positions() {
        // A project saved before tracks kept their colour.
        let mut data = project_with_tracks(3);
        for track in &mut data.tracks {
            track.color = None;
        }
        let mut sequencer = test_sequencer();
        sequencer.tracks_mut()[2].set_color_slot(7);

        data.apply_to_sequencer(&mut sequencer);

        assert_eq!(track_colors(&sequencer), vec![0, 1, 2]);
    }

    #[test]
    fn instrument_track_output_round_trips_through_json() {
        let original = TrackOutputData::Instrument {
            bundle_path: "/Library/Audio/Plug-Ins/CLAP/u-he/Repro-1.clap".to_string(),
            plugin_id: "com.u-he.Repro-5".to_string(),
            display_name: "Repro-5".to_string(),
            state: String::new(),
        };
        let json = serde_json::to_string(&original).unwrap();
        // No plugin state → the `state` key is skipped entirely.
        assert!(!json.contains("state"));
        let back: TrackOutputData = serde_json::from_str(&json).unwrap();
        match back {
            TrackOutputData::Instrument {
                bundle_path,
                plugin_id,
                display_name,
                state,
            } => {
                assert_eq!(
                    bundle_path,
                    "/Library/Audio/Plug-Ins/CLAP/u-he/Repro-1.clap"
                );
                assert_eq!(plugin_id, "com.u-he.Repro-5");
                assert_eq!(display_name, "Repro-5");
                assert!(state.is_empty());
            }
            other => panic!("expected Instrument, got {other:?}"),
        }
    }

    #[test]
    fn instrument_plugin_state_blob_round_trips_as_base64() {
        // Every byte value, including non-UTF-8 — a CLAP state blob is opaque
        // binary.
        let blob: Vec<u8> = (0u8..=255).chain(std::iter::once(7)).collect();
        let data = TrackOutputData::Instrument {
            bundle_path: "/x.clap".to_string(),
            plugin_id: "id".to_string(),
            display_name: "n".to_string(),
            state: BASE64.encode(&blob),
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains(r#""state""#));
        let back: TrackOutputData = serde_json::from_str(&json).unwrap();
        let TrackOutputData::Instrument { state, .. } = back else {
            panic!("expected Instrument");
        };
        assert_eq!(BASE64.decode(state).unwrap(), blob);
    }

    #[test]
    fn corrupt_instrument_state_decodes_to_empty_not_an_error() {
        // `apply_to_sequencer` must never fail the whole load over a bad blob.
        assert!(
            BASE64
                .decode("not valid base64!!!")
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[test]
    fn phase3_tag_only_instrument_output_deserializes_with_empty_fields() {
        // Projects saved by the phase-3 PoC wrote just `{"type":"Instrument"}`.
        let back: TrackOutputData = serde_json::from_str(r#"{"type":"Instrument"}"#).unwrap();
        match back {
            TrackOutputData::Instrument {
                bundle_path,
                plugin_id,
                display_name,
                state,
            } => {
                assert!(bundle_path.is_empty());
                assert!(plugin_id.is_empty());
                assert!(display_name.is_empty());
                assert!(state.is_empty());
            }
            other => panic!("expected Instrument, got {other:?}"),
        }
    }

    #[test]
    fn track_volume_pan_round_trip_through_json() {
        let track = TrackData {
            output: TrackOutputData::MidiOut { channel: 1 },
            volume_db: -6.5,
            pan: -0.4,
            muted: true,
            color: None,
            name: None,
            clips: Vec::new(),
        };
        let json = serde_json::to_string(&track).unwrap();
        let back: TrackData = serde_json::from_str(&json).unwrap();
        assert!((back.volume_db - -6.5).abs() < 1e-4);
        assert!((back.pan - -0.4).abs() < 1e-4);
        assert!(back.muted);
    }

    #[test]
    fn track_data_without_volume_pan_fields_defaults_to_neutral() {
        // A project saved before these fields existed.
        let back: TrackData =
            serde_json::from_str(r#"{"output":{"type":"MidiOut","channel":2},"clips":[]}"#)
                .unwrap();
        assert_eq!(back.volume_db, 0.0);
        assert_eq!(back.pan, 0.0);
        assert!(!back.muted);
        assert_eq!(back.color, None);
        assert_eq!(back.name, None);
    }

    #[test]
    fn default_project_data_resets_every_track_mix_to_neutral() {
        for track in ProjectData::default().tracks {
            assert_eq!(track.volume_db, 0.0);
            assert_eq!(track.pan, 0.0);
            assert!(!track.muted);
        }
    }

    #[test]
    fn default_project_data_resets_every_track_to_midi_out() {
        // `⌘N` applies `ProjectData::default()`; `apply_to_sequencer`'s loop is
        // bounded by `self.tracks.len()`, so a `default()` without an explicit
        // MIDI-Out entry per track would leave a track's previous CLAP
        // instrument output untouched instead of resetting it.
        let data = ProjectData::default();
        assert_eq!(data.tracks.len(), config::DEFAULT_TRACK_COUNT);
        for (i, track) in data.tracks.iter().enumerate() {
            assert!(track.clips.is_empty());
            match &track.output {
                TrackOutputData::MidiOut { channel } => assert_eq!(*channel, i as u8),
                other => panic!("expected MidiOut, got {other:?}"),
            }
        }
    }

    /// The DTO field names *are* the `.stev` JSON keys — there is no
    /// `#[serde(rename)]` indirection — so renaming a field silently changes
    /// the file format and makes every existing project fail to load with a
    /// missing-field error. These two were renamed from `start_ticks` / `ticks`
    /// on 2026-09-05 (with the projects on disk migrated in the same change);
    /// this test pins the result so the next rename has to be deliberate.
    #[test]
    fn clip_and_event_json_keys_are_the_on_disk_format() {
        let clip = ClipData {
            start_tick: 1_920,
            region_start: 0,
            region_end: 3_840,
            muted: false,
            swing_pct: 50,
            events: vec![EventData {
                tick: 480,
                length: 240,
                midi_message: vec![0x90, 60, 100],
                muted: false,
            }],
        };

        let json = serde_json::to_string(&clip).unwrap();
        assert!(json.contains(r#""start_tick":1920"#), "got {json}");
        assert!(json.contains(r#""tick":480"#), "got {json}");
        assert!(
            !json.contains("start_ticks"),
            "plural key is the old format"
        );

        // And the same keys read back.
        let back: ClipData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.start_tick, 1_920);
        assert_eq!(back.events[0].tick, 480);
    }

    /// `EventData::muted` was added after event mute existed on disk —
    /// `#[serde(default)]` keeps older projects (no `muted` key on an event)
    /// loading as unmuted rather than failing.
    #[test]
    fn event_muted_defaults_to_false_when_absent_from_older_json() {
        let json = r#"{"tick":0,"length":240,"midi_message":[144,60,100]}"#;
        let event: EventData = serde_json::from_str(json).unwrap();
        assert!(!event.muted);
    }

    #[test]
    fn event_muted_round_trips_through_json() {
        let event = EventData {
            tick: 0,
            length: 240,
            midi_message: vec![0x90, 60, 100],
            muted: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: EventData = serde_json::from_str(&json).unwrap();
        assert!(back.muted);
    }
}
