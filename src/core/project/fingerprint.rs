//! [`fingerprint`]: what a project would save, as one number, hashed straight
//! from the sequencer. The unsaved-changes check (`060-persistence.md`) and
//! the crash recovery check (`410-crash-recovery.md` decision 4) share it, so
//! a project has a recovery copy exactly when the quit prompt would ask. The
//! recovery check runs during playback, so it takes no snapshot and walks no
//! JSON: on a real project that is 0.06 ms against 1.65.

use std::hash::{DefaultHasher, Hash, Hasher};

use crate::core::sequencer::Sequencer;
use crate::models::track::TrackOutput;

/// A hash of everything `sequencer`'s project would write to disk but the
/// plugins' state blobs: two projects with the same fingerprint save the same
/// file, presets aside.
///
/// Every field of `ProjectData` belongs here: a new one is hashed too, and a
/// test stops the build until it is (`every_saved_field_is_listed`). The
/// blobs are left out because plenty of plugins write a different one on
/// every save of an unchanged preset (Nexus encrypts it afresh, DUNE
/// recompresses it), so they would make such a project always look changed.
/// Where the hand left each knob counts though it is never written
/// (`Track::knobs_by_hand`): a hand turn changes the plugin's state, which is
/// saved, while playback's never asks to save. So does each preset the plugin
/// says it loaded (`Track::plugin_changes`).
///
/// Stable within one run only — never persisted.
pub(crate) fn fingerprint(sequencer: &Sequencer) -> u64 {
    let mut hasher = DefaultHasher::new();
    let meter = sequencer.meter();
    (
        sequencer.tempo_us(),
        meter.numerator(),
        meter.denominator(),
        sequencer.region_start(),
        sequencer.region_end(),
        sequencer.tracks().len(),
    )
        .hash(&mut hasher);
    for (track_idx, track) in sequencer.tracks().iter().enumerate() {
        match track.output() {
            TrackOutput::MidiOut { channel } => channel.hash(&mut hasher),
            TrackOutput::Instrument(instrument) => (
                &instrument.bundle_path,
                &instrument.plugin_id,
                &instrument.display_name,
            )
                .hash(&mut hasher),
        }
        (
            sequencer.track_volume_db(track_idx).to_bits(),
            sequencer.track_pan(track_idx).to_bits(),
            sequencer.track_muted(track_idx),
            track.color_slot(),
            track.name(),
            track.plugin_changes(),
            track.clips().len(),
        )
            .hash(&mut hasher);
        for (param, by_hand) in track.knobs().iter().zip(track.knobs_by_hand()) {
            (param.map(|param| param.id), by_hand.map(f64::to_bits)).hash(&mut hasher);
        }
        for clip in track.clips() {
            let events = sequencer.saved_events(clip);
            (
                clip.start_tick(),
                clip.region().start(),
                clip.region().end(),
                clip.is_muted(),
                clip.swing_pct(),
                events.len(),
            )
                .hash(&mut hasher);
            for event in events {
                (
                    event.tick(),
                    event.end_tick(),
                    event.midi_message(),
                    event.is_muted(),
                    event
                        .param_move()
                        .map(|moved| (moved.param, moved.value.to_bits())),
                )
                    .hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use crate::core::project::dto::{
        ClipData, EventData, KnobData, MeterData, ParamMoveData, ProjectData, TrackData,
        TrackOutputData,
    };
    use crate::core::sequencer::test_support::{
        clip_at, note_off, note_on, param_at, stub_instrument, test_sequencer,
    };
    use crate::core::time::Meter;
    use crate::models::clip::Clip;
    use crate::models::event::Event;
    use crate::models::knobs::{PluginParam, UNASSIGNED_KNOBS, test_param};
    use crate::models::track::InstrumentRef;

    use super::*;

    /// The fingerprint as it was before [`fingerprint`] replaced it: the
    /// snapshot's JSON, blobs left out, then what the JSON never holds. What
    /// the direct hash is tested against.
    fn json_hash(sequencer: &Sequencer) -> u64 {
        let mut data = ProjectData::from_sequencer(sequencer);
        for track in &mut data.tracks {
            if let TrackOutputData::Instrument { state, .. } = &mut track.output {
                state.clear();
            }
        }
        let mut hasher = DefaultHasher::new();
        hasher.write(serde_json::to_string(&data).unwrap().as_bytes());
        for track in sequencer.tracks() {
            for by_hand in track.knobs_by_hand() {
                hasher.write_u64(by_hand.map_or(u64::MAX, f64::to_bits));
            }
            hasher.write_u32(track.plugin_changes());
        }
        hasher.finish()
    }

    /// The events [`project`]'s clip starts with.
    fn events() -> Vec<Event> {
        vec![note_on(0), note_off(240), param_at(10, 714, 0.25)]
    }

    /// A project with something in every field: a clip with notes and a
    /// parameter move on track 0, a plugin on track 1, a knob assigned.
    fn project() -> Sequencer {
        let mut sequencer = test_sequencer();
        let mut clip = clip_at(0, 1920);
        clip.restore_events(events());
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.tracks_mut()[1].set_output(stub_instrument(vec![1, 2, 3]));
        let mut knobs = UNASSIGNED_KNOBS;
        knobs[0] = Some(test_param(714));
        sequencer.tracks_mut()[1].set_knobs(knobs);
        sequencer
    }

    /// Runs `edit` on [`project`]'s clip.
    fn edit_clip(sequencer: &mut Sequencer, edit: impl FnOnce(&mut Clip)) {
        let id = sequencer.tracks()[0].clips()[0].id();
        edit(sequencer.tracks_mut()[0].get_clip_by_id_mut(id).unwrap());
    }

    /// Gives [`project`]'s clip its [`events`] with `edit` made to them.
    fn edit_events(sequencer: &mut Sequencer, edit: impl FnOnce(&mut Vec<Event>)) {
        let mut list = events();
        edit(&mut list);
        edit_clip(sequencer, |clip| clip.restore_events(list));
    }

    /// Replaces the plugin on [`project`]'s track 1 with `edit` of it.
    fn edit_plugin(sequencer: &mut Sequencer, edit: impl FnOnce(&mut InstrumentRef)) {
        let track = &mut sequencer.tracks_mut()[1];
        let mut output = track.output().clone();
        if let TrackOutput::Instrument(instrument) = &mut output {
            edit(instrument);
        }
        track.set_output(output);
    }

    /// One edit of [`project`], named for the failure message.
    type Change = (&'static str, fn(&mut Sequencer));

    /// One edit for each thing a project saves.
    const SAVED_CHANGES: &[Change] = &[
        ("tempo", |s| s.set_tempo(400_000)),
        ("meter", |s| s.set_meter(Meter::new(3, 4).unwrap())),
        ("region start", |s| s.set_global_region(480, s.region_end())),
        ("region end", |s| s.set_global_region(0, 1920)),
        ("track count", |s| s.set_track_count(5)),
        ("channel", |s| {
            s.tracks_mut()[0].set_output(TrackOutput::MidiOut { channel: 9 });
        }),
        ("a plugin in place of MIDI", |s| {
            s.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
        }),
        ("bundle path", |s| {
            edit_plugin(s, |i| i.bundle_path = "/y.clap".into());
        }),
        ("plugin id", |s| {
            edit_plugin(s, |i| i.plugin_id = "b".into())
        }),
        ("plugin name", |s| {
            edit_plugin(s, |i| i.display_name = "B".into());
        }),
        ("volume", |s| s.set_track_volume(0, -3.0)),
        ("pan", |s| s.set_track_pan(0, 0.5)),
        ("track mute", |s| s.set_track_muted(0, true)),
        ("colour", |s| s.tracks_mut()[0].set_color_slot(7)),
        ("track name", |s| {
            s.tracks_mut()[1].set_name(Some("Bass".to_owned()));
        }),
        ("knob", |s| {
            let mut knobs = UNASSIGNED_KNOBS;
            knobs[1] = Some(test_param(714));
            s.tracks_mut()[1].set_knobs(knobs);
        }),
        ("a hand turn", |s| s.turn_knob(0, 0, 0.25, 0)),
        ("a preset loaded", |s| s.count_plugin_change(0)),
        ("another clip", |s| {
            s.tracks_mut()[0].add_clip(&clip_at(3840, 1920));
        }),
        ("clip start", |s| edit_clip(s, |c| c.set_start_tick(480))),
        ("clip region start", |s| {
            edit_clip(s, |c| {
                c.region_mut().set_region(Some(10), Some(1920));
            });
        }),
        ("clip region end", |s| {
            edit_clip(s, |c| {
                c.region_mut().set_region(Some(0), Some(960));
            });
        }),
        ("clip mute", |s| edit_clip(s, |c| c.set_muted(true))),
        ("swing", |s| edit_clip(s, |c| c.set_swing_pct(60))),
        ("event tick", |s| edit_events(s, |e| e[0] = note_on(5))),
        ("note length", |s| edit_events(s, |e| e[0].set_length(120))),
        ("event bytes", |s| {
            edit_events(s, |e| e[0] = Event::new(0, 0, vec![0x90, 61, 100]));
        }),
        ("event mute", |s| edit_events(s, |e| e[0].set_muted(true))),
        ("move value", |s| {
            edit_events(s, |e| e[2] = param_at(10, 714, 0.75));
        }),
        ("move parameter", |s| {
            edit_events(s, |e| e[2] = param_at(10, 715, 0.25));
        }),
        ("move plugin", |s| {
            edit_events(s, |e| e[2].set_param(PluginParam { plugin: 2, id: 714 }));
        }),
        ("one event fewer", |s| {
            edit_events(s, |e| {
                e.pop();
            });
        }),
    ];

    /// What a project doesn't save, or that the check leaves out.
    const UNSAVED_CHANGES: &[Change] = &[
        ("cursor", |s| s.set_cursor_tick(480)),
        ("solo", |s| s.set_track_soloed(0, true)),
        ("plugin state", |s| {
            edit_plugin(s, |i| i.state = [9, 9].into())
        }),
    ];

    /// The same project hashes the same, however it was built.
    #[test]
    fn a_project_fingerprints_the_same_each_time() {
        assert_eq!(fingerprint(&project()), fingerprint(&project()));
    }

    /// Each thing a project saves changes both the direct hash and the JSON
    /// one (`410-crash-recovery.md` decision 4).
    #[test]
    fn the_direct_hash_sees_every_change_the_json_does() {
        let direct = fingerprint(&project());
        let json = json_hash(&project());
        for (name, change) in SAVED_CHANGES {
            let mut sequencer = project();
            change(&mut sequencer);
            assert_ne!(fingerprint(&sequencer), direct, "direct: {name}");
            assert_ne!(json_hash(&sequencer), json, "json: {name}");
        }
    }

    /// Transient state changes neither hash, and nor does a state blob: a
    /// plugin that writes a different one for the same preset on every save
    /// must not make the project look changed.
    #[test]
    fn neither_hash_sees_what_a_save_leaves_out() {
        let direct = fingerprint(&project());
        let json = json_hash(&project());
        for (name, change) in UNSAVED_CHANGES {
            let mut sequencer = project();
            change(&mut sequencer);
            assert_eq!(fingerprint(&sequencer), direct, "direct: {name}");
            assert_eq!(json_hash(&sequencer), json, "json: {name}");
        }
    }

    /// A count or a value that goes back goes back to the same hash, so
    /// undoing to the saved state counts as no change.
    #[test]
    fn undoing_a_change_restores_the_fingerprint() {
        let sequencer = project();
        let saved = fingerprint(&sequencer);
        let tempo = sequencer.tempo_us();
        sequencer.set_tempo(tempo + 1);
        assert_ne!(fingerprint(&sequencer), saved);
        sequencer.set_tempo(tempo);
        assert_eq!(fingerprint(&sequencer), saved);
    }

    /// Every field the project file has, named without a `..`: a new one
    /// stops this compiling until it is added here, and to [`fingerprint`]
    /// and `SAVED_CHANGES` with it. Never called.
    #[allow(dead_code)]
    fn every_saved_field_is_listed(data: ProjectData) {
        let ProjectData {
            tempo_us: _,
            meter,
            region_start: _,
            region_end: _,
            tracks,
        } = data;
        let [
            MeterData {
                bar_index: _,
                numerator: _,
                denominator: _,
            },
        ] = meter[..]
        else {
            return;
        };
        for track in tracks {
            let TrackData {
                output,
                volume_db: _,
                pan: _,
                muted: _,
                color: _,
                name: _,
                knobs,
                clips,
            } = track;
            match output {
                TrackOutputData::MidiOut { channel: _ } => {}
                TrackOutputData::Instrument {
                    bundle_path: _,
                    plugin_id: _,
                    display_name: _,
                    state: _,
                } => {}
            }
            for KnobData { param: _ } in knobs {}
            for clip in clips {
                let ClipData {
                    start_tick: _,
                    region_start: _,
                    region_end: _,
                    muted: _,
                    swing_pct: _,
                    events,
                } = clip;
                for event in events {
                    let EventData {
                        tick: _,
                        length: _,
                        midi_message: _,
                        param_move,
                        muted: _,
                    } = event;
                    if let Some(ParamMoveData {
                        param: _,
                        plugin: _,
                        value: _,
                    }) = param_move
                    {}
                }
            }
        }
    }
}
