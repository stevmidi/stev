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

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use log::warn;
use serde::{Deserialize, Serialize};

use crate::{
    core::{config, sequencer::Sequencer, time::Meter},
    metadata::clip_metadata::ClipMetadata,
    models::{
        clip::Clip,
        event::{Event, ParamMove},
        knobs::{ParamId, PluginParam, PluginTag, knobs_from_saved},
        track::{InstrumentRef, TrackOutput, track_name_from_input},
    },
};

/// A plugin parameter's recorded move, on disk (`400-parameter-knobs.md`
/// decision 11).
#[derive(Serialize, Deserialize, Clone, Copy)]
pub(crate) struct ParamMoveData {
    /// The parameter's ID in its plugin.
    pub(crate) param: ParamId,
    /// The plugin it was recorded on ([`plugin_tag`](crate::models::knobs::plugin_tag)),
    /// which a clip moved to another track keeps. Absent means the track's
    /// own plugin, as in a project saved before moves named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plugin: Option<PluginTag>,
    /// Its normalized value, `0.0..=1.0`.
    pub(crate) value: f64,
}

/// One event, on disk: MIDI, or a parameter move. `tick` is an event-tick
/// position; `length` an amount (ticks to the paired `NoteOff`).
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct EventData {
    /// Event-tick position within the clip.
    pub(crate) tick: i32,
    /// Note length in ticks (`NoteOn` only; `0` otherwise).
    pub(crate) length: i32,
    /// Raw MIDI bytes; none for a parameter move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) midi_message: Vec<u8>,
    /// The parameter move this event is. Absent on every MIDI event, and so
    /// in every project saved before parameter moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) param_move: Option<ParamMoveData>,
    /// Whether the event is muted. Absent in projects saved before per-event
    /// mute existed — `#[serde(default)]` gives them the unmuted default.
    #[serde(default)]
    pub(crate) muted: bool,
}

impl EventData {
    /// `event` as a save writes it.
    fn of(event: &Event) -> Self {
        EventData {
            tick: event.tick(),
            length: event.end_tick() - event.tick(),
            midi_message: event.midi_message().to_vec(),
            param_move: event.param_move().map(|moved| ParamMoveData {
                param: moved.param.id,
                plugin: Some(moved.param.plugin),
                value: moved.value,
            }),
            muted: event.is_muted(),
        }
    }
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
                state: BASE64.decode(state).unwrap_or_default().into(),
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
    /// `0.0` = centre). `#[serde(default)]` (`0.0`, not
    /// [`config::TRACK_VOLUME_DB_DEFAULT`]): a project saved before this
    /// field existed was at unity/centre and loads as it was. See
    /// `060-persistence.md`.
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
    /// What the track's eight knobs drive (`Track::knobs`), in reading
    /// order. Not written while no knob is assigned; empty then, in a
    /// project saved before knobs, and in `ProjectData::default`. A list no
    /// track could have loads unassigned (`knobs_from_saved`), and so does a
    /// project saved with CC knobs: their entries name no parameter, and
    /// their CC numbers and positions are ignored (`400-parameter-knobs.md`
    /// decision 13).
    #[serde(default, skip_serializing_if = "no_knob_assigned")]
    pub(crate) knobs: Vec<KnobData>,
    /// The track's clips.
    pub(crate) clips: Vec<ClipData>,
}

impl TrackData {
    /// Sets the plugin's state blob from `raw`; no-op on a track with no plugin.
    fn set_state(&mut self, raw: &[u8]) {
        if let TrackOutputData::Instrument { state, .. } = &mut self.output {
            *state = BASE64.encode(raw);
        }
    }
}

/// One knob, on disk.
#[derive(Serialize, Deserialize, Clone, Copy)]
pub(crate) struct KnobData {
    /// The plugin parameter it drives, by ID; absent while unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) param: Option<ParamId>,
}

/// Whether no knob in `knobs` is assigned — the list isn't written then.
fn no_knob_assigned(knobs: &[KnobData]) -> bool {
    knobs.iter().all(|knob| knob.param.is_none())
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
        warn!(
            "Unsupported meter {}/{} in project, using 4/4",
            first.numerator, first.denominator
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
                    volume_db: config::TRACK_VOLUME_DB_DEFAULT,
                    pan: 0.0,
                    muted: false,
                    color: Some(i),
                    name: None,
                    knobs: Vec::new(),
                    clips: Vec::new(),
                })
                .collect(),
        }
    }
}

/// A project taken off the sequencer with its plugin state and its clips'
/// events still as the sequencer holds them, shared, not copied: the
/// [`ProjectData`] a save writes, short of the base64 and the events'
/// [`EventData`]. So the `"sequencer"` thread takes this and the `"storage"`
/// thread does the rest, for a save and for the recovery copy
/// (`420-large-projects.md` decisions 3, 7 and 8, `410-crash-recovery.md`
/// decision 5).
pub(crate) struct ProjectSnapshot {
    /// The project, every state blob empty and every clip without events.
    data: ProjectData,
    /// Each clip's events, by track and clip in the order of `data`. Shared
    /// with the clips (`Clip::shared_events`).
    events: Vec<Vec<Arc<Vec<Event>>>>,
    /// Each track's raw plugin state, in track order; empty on a track with
    /// no plugin. Shared with the tracks, not copied
    /// (`420-large-projects.md` decision 7).
    states: Vec<Arc<[u8]>>,
}

impl ProjectSnapshot {
    /// `sequencer`'s project as it stands.
    pub(crate) fn of(sequencer: &Sequencer) -> Self {
        Self::taken(sequencer, true)
    }

    /// `sequencer`'s project with no clips, for a template, which keeps none
    /// (`ProjectData::into_template`): the events aren't copied only to be
    /// dropped.
    pub(crate) fn template_of(sequencer: &Sequencer) -> Self {
        Self::taken(sequencer, false)
    }

    /// `sequencer`'s project, its clips too when `with_clips`.
    fn taken(sequencer: &Sequencer, with_clips: bool) -> Self {
        ProjectSnapshot {
            data: ProjectData::without_state(sequencer, with_clips),
            events: sequencer
                .tracks()
                .iter()
                .map(|track| {
                    track
                        .clips()
                        .iter()
                        .filter(|_| with_clips)
                        .map(|clip| Arc::clone(sequencer.saved_events(clip)))
                        .collect()
                })
                .collect(),
            states: sequencer
                .tracks()
                .iter()
                .map(|track| {
                    track
                        .output()
                        .instrument()
                        .map_or_else(Arc::default, |instrument| Arc::clone(&instrument.state))
                })
                .collect(),
        }
    }

    /// The whole project, each state blob in base64.
    pub(crate) fn encoded(self) -> ProjectData {
        let ProjectSnapshot {
            mut data,
            events,
            states,
        } = self;
        for ((track, raw), events) in data.tracks.iter_mut().zip(states).zip(events) {
            track.set_state(&raw);
            for (clip, events) in track.clips.iter_mut().zip(events) {
                clip.events = events.iter().map(EventData::of).collect();
            }
        }
        data
    }
}

impl ProjectData {
    /// This project as a template: what describes the instruments kept (each
    /// track's output and plugin state, name, colour, volume and pan; the
    /// track count, meter and tempo), what describes a performance or a
    /// passing state reset — no clips, no track muted, the default loop
    /// region. Applied when a template opens, so one holding clips (a project
    /// copied into the folder by hand) still opens empty, and when one is
    /// saved. See `archive/340-templates.md` decisions 5 and 6.
    pub(crate) fn into_template(mut self) -> Self {
        self.region_start = 0;
        self.region_end = config::REGION_LENGTH_DEFAULT;
        for track in &mut self.tracks {
            track.muted = false;
            track.clips.clear();
        }
        self
    }

    /// The sequencer's project as a save writes it, at once: for tests. A
    /// save takes a [`ProjectSnapshot`] and encodes it on the `"storage"`
    /// thread (`420-large-projects.md` decision 3).
    #[cfg(test)]
    pub(crate) fn from_sequencer(sequencer: &Sequencer) -> Self {
        ProjectSnapshot::of(sequencer).encoded()
    }

    /// The sequencer's project with every state blob and every clip's events
    /// left empty, for [`ProjectSnapshot`] to fill in; with no clips unless
    /// `with_clips`.
    fn without_state(sequencer: &Sequencer, with_clips: bool) -> Self {
        let tracks = sequencer
            .tracks()
            .iter()
            .enumerate()
            .map(|(track_idx, track)| {
                let clips = track
                    .clips()
                    .iter()
                    .filter(|_| with_clips)
                    .map(|clip| ClipData {
                        start_tick: clip.start_tick(),
                        region_start: clip.region().start(),
                        region_end: clip.region().end(),
                        muted: clip.is_muted(),
                        swing_pct: clip.swing_pct(),
                        events: Vec::new(),
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
                            state: String::new(),
                        },
                    },
                    volume_db: sequencer.track_volume_db(track_idx),
                    pan: sequencer.track_pan(track_idx),
                    muted: sequencer.track_muted(track_idx),
                    color: Some(track.color_slot()),
                    name: track.name().map(str::to_owned),
                    knobs: track
                        .knobs()
                        .iter()
                        .map(|param| KnobData {
                            param: param.map(|param| param.id),
                        })
                        .collect(),
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
                let own_plugin = track.output().plugin_tag();
                let saved_knobs: Vec<Option<ParamId>> =
                    track_data.knobs.iter().map(|knob| knob.param).collect();
                track.set_knobs(knobs_from_saved(&saved_knobs, own_plugin));

                for clip_data in track_data.clips {
                    let mut clip = Clip::new();
                    clip.set_start_tick(clip_data.start_tick);
                    clip.region_mut()
                        .set_region(Some(clip_data.region_start), Some(clip_data.region_end));
                    clip.set_muted(clip_data.muted);
                    clip.set_swing_pct(clip_data.swing_pct);

                    // Add events (delta_ticks recalculated by add_event)
                    for event_data in clip_data.events {
                        let mut event = match event_data.param_move {
                            Some(ParamMoveData {
                                param,
                                plugin,
                                value,
                            }) => {
                                let param = PluginParam {
                                    plugin: plugin.unwrap_or(own_plugin),
                                    id: param,
                                };
                                Event::from_param_move(event_data.tick, ParamMove { param, value })
                            }
                            None => Event::new(
                                event_data.tick,
                                event_data.length,
                                event_data.midi_message,
                            ),
                        };
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
    use crate::core::sequencer::test_support::{
        clip_at, note_off, note_on, param_at, param_moves, stub_instrument, test_sequencer,
        track_colors,
    };
    use crate::models::knobs::{KnobParams, UNASSIGNED_KNOBS, test_param};

    /// A default project with `count` tracks.
    fn project_with_tracks(count: usize) -> ProjectData {
        let mut data = ProjectData::default();
        let track = data.tracks[0].clone();
        data.tracks = vec![track; count];
        data
    }

    /// A template keeps the instruments and their mix, and drops the
    /// performance: clips, mutes and the loop region.
    #[test]
    fn a_template_keeps_the_setup_and_drops_the_performance() {
        let mut data = ProjectData {
            tempo_us: 666_667,
            meter: MeterData::list_for(Meter::new(6, 8).unwrap()),
            region_start: 1920,
            region_end: 7680,
            ..ProjectData::default()
        };
        let track = &mut data.tracks[1];
        track.output = TrackOutputData::Instrument {
            bundle_path: "/Synth.vst3".to_owned(),
            plugin_id: "synth".to_owned(),
            display_name: "Synth".to_owned(),
            state: "AAAA".to_owned(),
        };
        track.volume_db = -3.0;
        track.pan = 0.5;
        track.muted = true;
        track.color = Some(7);
        track.name = Some("Pad".to_owned());
        track.clips.push(ClipData {
            start_tick: 0,
            region_start: 0,
            region_end: 1920,
            muted: false,
            swing_pct: 50,
            events: Vec::new(),
        });
        let mut expected = data.clone();
        expected.region_start = 0;
        expected.region_end = config::REGION_LENGTH_DEFAULT;
        expected.tracks[1].muted = false;
        expected.tracks[1].clips.clear();

        assert_eq!(
            serde_json::to_string(&data.into_template()).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
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
    fn knob_assignments_survive_a_save_and_load_and_a_new_project_resets_them() {
        let mut sequencer = test_sequencer();
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[2] = Some(test_param(714));
        knobs[7] = Some(test_param(7_000_000));
        sequencer.set_track_knobs(1, knobs);
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();

        sequencer.new_project();
        assert_eq!(*sequencer.tracks()[1].knobs(), UNASSIGNED_KNOBS);
        let data: ProjectData = serde_json::from_str(&json).unwrap();
        data.apply_to_sequencer(&mut sequencer);

        let ids = |knobs: &KnobParams| knobs.map(|knob| knob.map(|knob| knob.id));
        assert_eq!(ids(sequencer.tracks()[1].knobs()), ids(&knobs));
        assert_eq!(*sequencer.tracks()[0].knobs(), UNASSIGNED_KNOBS);
    }

    /// A clip's parameter moves survive a save and a load, muted ones too;
    /// the notes beside them keep their bytes.
    #[test]
    fn a_parameter_move_round_trips() {
        let mut sequencer = test_sequencer();
        let mut clip = clip_at(0, 1920);
        let mut muted = param_at(40, 714, 0.5);
        muted.set_muted(true);
        clip.restore_events(vec![param_at(10, 714, 0.25), muted, note_on(20)]);
        sequencer.tracks_mut()[0].add_clip(&clip);

        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();
        let data: ProjectData = serde_json::from_str(&json).unwrap();
        let mut loaded = test_sequencer();
        data.apply_to_sequencer(&mut loaded);

        let events = loaded.tracks()[0].clips()[0].events();
        assert_eq!(param_moves(events), vec![(10, 714, 0.25), (40, 714, 0.5)]);
        assert!(events[2].is_muted());
        assert_eq!(events[1].midi_message(), &[0x90, 60, 100]);
    }

    /// A move keeps the plugin it was recorded on through a save, on a track
    /// with another; one saved naming none is the track's own plugin's.
    #[test]
    fn a_parameter_move_loads_with_its_plugin() {
        let mut sequencer = test_sequencer();
        let mut clip = clip_at(0, 1920);
        clip.restore_events(vec![param_at(10, 714, 0.25)]);
        sequencer.tracks_mut()[0].add_clip(&clip);
        let plugin_after_load = |data: ProjectData| {
            let mut loaded = test_sequencer();
            data.apply_to_sequencer(&mut loaded);
            let event = &loaded.tracks()[0].clips()[0].events()[0];
            event.param_move().unwrap().param.plugin
        };

        let data = ProjectData::from_sequencer(&sequencer);
        assert_eq!(plugin_after_load(data), test_param(714).plugin);

        let mut unnamed = ProjectData::from_sequencer(&sequencer);
        let event = &mut unnamed.tracks[0].clips[0].events[0];
        event.param_move.as_mut().unwrap().plugin = None;
        assert_eq!(
            plugin_after_load(unnamed),
            sequencer.tracks()[0].output().plugin_tag()
        );
    }

    /// A MIDI event writes no `param_move`, so a project with none reads as
    /// it always did.
    #[test]
    fn a_midi_event_writes_no_param_move() {
        let event: EventData =
            serde_json::from_str(r#"{"tick":0,"length":240,"midi_message":[144,60,100]}"#).unwrap();
        assert!(event.param_move.is_none());
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("param_move")
        );
    }

    /// A track with no knob assigned writes no knob list, and an assigned
    /// one writes only what it drives.
    #[test]
    fn unassigned_knobs_are_not_written() {
        let mut sequencer = test_sequencer();
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();
        assert!(!json.contains("knobs"), "{json}");
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[1] = Some(test_param(3));
        sequencer.set_track_knobs(0, knobs);
        let json = serde_json::to_string(&ProjectData::from_sequencer(&sequencer)).unwrap();
        assert!(
            json.contains(r#""knobs":[{},{"param":3},{},{},{},{},{},{}]"#),
            "{json}"
        );
    }

    /// A project saved with CC knobs (`390`) loads with its knobs
    /// unassigned: the CC numbers and positions are ignored
    /// (`400-parameter-knobs.md` decision 13). So does one saved before
    /// knobs.
    #[test]
    fn an_old_projects_cc_knobs_load_unassigned() {
        let cc_knobs = (14..22)
            .map(|cc| format!(r#"{{"cc":{cc},"value":64}}"#))
            .collect::<Vec<_>>()
            .join(",");
        for json in [
            format!(
                r#"{{"output":{{"type":"MidiOut","channel":2}},"knobs":[{cc_knobs}],"clips":[]}}"#
            ),
            r#"{"output":{"type":"MidiOut","channel":2},"clips":[]}"#.to_owned(),
        ] {
            let back: TrackData = serde_json::from_str(&json).unwrap();
            let data = ProjectData {
                tracks: vec![back],
                ..ProjectData::default()
            };
            let mut sequencer = test_sequencer();
            let mut knobs = UNASSIGNED_KNOBS;
            knobs[0] = Some(test_param(1));
            sequencer.set_track_knobs(0, knobs);
            data.apply_to_sequencer(&mut sequencer);
            assert_eq!(*sequencer.tracks()[0].knobs(), UNASSIGNED_KNOBS);
        }
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

    /// A snapshot shares each track's plugin state rather than copying it
    /// (`420-large-projects.md` decision 7), and still encodes it.
    #[test]
    fn a_snapshot_shares_the_plugin_state_and_encodes_it() {
        let mut sequencer = test_sequencer();
        sequencer.set_track_output(1, stub_instrument(vec![1, 2, 3]));
        let instrument = sequencer.tracks()[1].output().instrument().unwrap();
        let snapshot = ProjectSnapshot::of(&sequencer);
        assert!(Arc::ptr_eq(&snapshot.states[1], &instrument.state));
        assert!(snapshot.states[0].is_empty());

        let TrackOutputData::Instrument { state, .. } = &snapshot.encoded().tracks[1].output else {
            panic!("expected Instrument");
        };
        assert_eq!(BASE64.decode(state).unwrap(), [1, 2, 3]);
    }

    /// A snapshot shares each clip's events rather than copying them
    /// (`420-large-projects.md` decision 8), and still encodes them; a
    /// template's has none.
    #[test]
    fn a_snapshot_shares_the_clips_events_and_encodes_them() {
        let mut sequencer = test_sequencer();
        let mut clip = clip_at(0, 1920);
        clip.restore_events(vec![note_on(0), note_off(240)]);
        clip.calculate_note_lengths();
        sequencer.tracks_mut()[1].add_clip(&clip);
        let events = sequencer.tracks()[1].clips()[0].shared_events();
        let snapshot = ProjectSnapshot::of(&sequencer);
        assert!(Arc::ptr_eq(&snapshot.events[1][0], events));
        assert!(snapshot.events[0].is_empty());
        assert!(snapshot.data.tracks[1].clips[0].events.is_empty());

        let data = snapshot.encoded();
        let ticks: Vec<_> = data.tracks[1].clips[0]
            .events
            .iter()
            .map(|event| (event.tick, event.length))
            .collect();
        assert_eq!(ticks, [(0, 240), (240, 0)]);

        let template = ProjectSnapshot::template_of(&sequencer);
        assert!(template.events.iter().all(Vec::is_empty));
        assert!(template.encoded().tracks[1].clips.is_empty());
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
            knobs: Vec::new(),
            clips: Vec::new(),
        };
        let json = serde_json::to_string(&track).unwrap();
        let back: TrackData = serde_json::from_str(&json).unwrap();
        assert!((back.volume_db - -6.5).abs() < 1e-4);
        assert!((back.pan - -0.4).abs() < 1e-4);
        assert!(back.muted);
    }

    #[test]
    fn track_data_without_volume_pan_fields_loads_at_unity_and_centre() {
        // A project saved before these fields existed was at unity — not at
        // a fresh track's `TRACK_VOLUME_DB_DEFAULT` — and loads as it was.
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
    fn default_project_data_starts_every_track_at_a_fresh_mix() {
        for track in ProjectData::default().tracks {
            assert_eq!(track.volume_db, config::TRACK_VOLUME_DB_DEFAULT);
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
                param_move: None,
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
            param_move: None,
            muted: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: EventData = serde_json::from_str(&json).unwrap();
        assert!(back.muted);
    }
}
