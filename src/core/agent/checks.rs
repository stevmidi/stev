//! The checking tool (`360-mcp-server.md` § Checking: `check_notes`): a
//! dry run of `create_clips` or `replace_clip_notes`. It reads the call
//! exactly as the write would (`writes.rs`'s prepare step, unhumanised), so
//! the rows it shows are the ones the write would put down, then reports
//! what the model can't hear: notes outside the pitch classes the other
//! tracks sound at their beat, what the cleanup would change, same-pitch
//! overlaps the write would trim, and existing clips in the way. Nothing is
//! written, and it runs whatever holds the edit (decision 13 is for writes).

use std::collections::HashMap;

use serde_json::Value;

use crate::core::sequencer::Sequencer;
use crate::core::time::Meter;
use crate::models::clip::Clip;

use super::reads::{
    PlayedNote, beat_span, clip_span, pitch_classes, short_id, track_played, track_title,
};
use super::units::{NoteRow, counted, format_position, note_name, pitch_class_name};
use super::writes::{Cleaned, PreparedClips, PreparedReplace, prepare_clips, prepare_replace};

/// The most findings one check lists; the rest are counted.
const MAX_FINDINGS: usize = 40;

/// One part a write would put down, in arrangement ticks.
struct Part {
    /// The track it goes on.
    track_idx: usize,
    /// How the findings name the track: `track 1 "bass"`.
    track: String,
    /// Where it starts: the tick its rows' starts count from.
    start: i32,
    /// Where it ends.
    end: i32,
    /// The rows as the write would put them, starts from `start`.
    rows: Vec<NoteRow>,
    /// How the reply heads its rows: `Clip 1, track 1 "bass", 17 → 21`.
    heading: String,
}

impl Part {
    /// The part `clip` on `track_idx` holds as `rows` (from its start),
    /// named `Clip <label>`.
    fn new(
        sequencer: &Sequencer,
        label: String,
        track_idx: usize,
        clip: &Clip,
        rows: Vec<NoteRow>,
    ) -> Part {
        let track = track_title(track_idx, &sequencer.tracks()[track_idx]);
        Part {
            heading: format!(
                "Clip {label}, {track}, {}",
                clip_span(clip, sequencer.meter())
            ),
            track_idx,
            track,
            start: clip.start_tick(),
            end: clip.end_tick(),
            rows,
        }
    }
}

/// `check_notes`: the call checked as `replace_clip_notes` when it names a
/// `clip`, else as `create_clips`.
pub(super) fn check_notes(sequencer: &Sequencer, args: &Value) -> Result<String, String> {
    let meter = sequencer.meter();
    let (parts, cleaned, in_the_way): (Vec<Part>, Cleaned, Vec<String>) =
        if args.get("clip").is_some() {
            let PreparedReplace {
                track_idx,
                clip,
                rows,
                cleaned,
            } = prepare_replace(sequencer, args, None)?;
            let rows = rebased(rows, clip.region().start());
            let part = Part::new(sequencer, short_id(clip), track_idx, clip, rows);
            (vec![part], cleaned, Vec::new())
        } else {
            let PreparedClips {
                clips,
                cleaned,
                in_the_way,
            } = prepare_clips(sequencer, args, None)?;
            let parts = clips
                .into_iter()
                .enumerate()
                .map(|(i, new)| {
                    Part::new(
                        sequencer,
                        (i + 1).to_string(),
                        new.track_idx,
                        &new.clip,
                        new.rows,
                    )
                })
                .collect();
            (parts, cleaned, in_the_way)
        };

    let mut lines = vec![format!(
        "Dry run, nothing written. The rows as the write would put them{}:",
        if cleaned.humanize() {
            ", before its humanising"
        } else {
            ""
        }
    )];
    for part in &parts {
        lines.push(format!("{}:", part.heading));
        lines.extend(part.rows.iter().map(NoteRow::format));
    }

    let mut findings = clashes(sequencer, &parts, meter);
    findings.extend(overlaps(&parts, meter));
    findings.extend(cleaned.changes());
    findings.extend(in_the_way.into_iter().map(|clip| {
        format!("{clip}: create_clips refuses to write over it unless replace_existing.")
    }));

    if findings.is_empty() {
        lines.push("No findings.".to_owned());
    } else {
        lines.push("Findings:".to_owned());
        let more = findings.len().saturating_sub(MAX_FINDINGS);
        lines.extend(findings.into_iter().take(MAX_FINDINGS));
        if more > 0 {
            lines.push(format!("… and {} more.", counted(more, "finding")));
        }
    }
    Ok(lines.join("\n"))
}

/// `rows` in a clip's event ticks as rows from its window start `origin`.
fn rebased(rows: Vec<NoteRow>, origin: i32) -> Vec<NoteRow> {
    rows.into_iter()
        .map(|row| NoteRow {
            start: row.start - origin,
            ..row
        })
        .collect()
}

/// What sounds on each track inside the parts once the write is done: a
/// track's own notes there, those starting inside a part on it replaced by
/// the part's. Tracks that aren't heard sound nothing.
fn sounding_after(sequencer: &Sequencer, parts: &[Part]) -> Vec<Vec<PlayedNote>> {
    let from = parts.iter().map(|part| part.start).min().unwrap_or(0);
    let to = parts.iter().map(|part| part.end).max().unwrap_or(0);
    let audible = sequencer.track_audibility();
    sequencer
        .tracks()
        .iter()
        .enumerate()
        .map(|(idx, track)| {
            if !audible[idx] {
                return Vec::new();
            }
            let on_track: Vec<&Part> = parts.iter().filter(|part| part.track_idx == idx).collect();
            let mut notes: Vec<PlayedNote> = track_played(track, from, to)
                .0
                .into_iter()
                .filter(|note| {
                    parts.iter().any(|part| note.overlaps(part.start, part.end))
                        && !on_track
                            .iter()
                            .any(|part| (part.start..part.end).contains(&note.start))
                })
                .collect();
            for part in on_track {
                notes.extend(
                    part.rows
                        .iter()
                        .map(|row| PlayedNote::from_row(row, part.start)),
                );
            }
            notes
        })
        .collect()
}

/// One line per written note whose pitch class none of the other tracks
/// sounds in its beat, while they sound something: `F#4 at 18+2.5 on
/// track 1 "bass": sounding A C E`.
fn clashes(sequencer: &Sequencer, parts: &[Part], meter: Meter) -> Vec<String> {
    let sounding = sounding_after(sequencer, parts);
    // What the other tracks sound in a beat, worked out once per beat and
    // written track: rows sharing a beat share the answer.
    let mut by_beat: HashMap<(i32, usize), Vec<&'static str>> = HashMap::new();
    let mut findings = Vec::new();
    for part in parts {
        for row in &part.rows {
            let tick = part.start + row.start;
            let (beat, beat_end) = beat_span(tick, meter);
            let classes = by_beat.entry((beat, part.track_idx)).or_insert_with(|| {
                let mut others: Vec<i32> = sounding
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| *idx != part.track_idx)
                    .flat_map(|(_, notes)| notes)
                    .filter(|note| note.overlaps(beat, beat_end))
                    .map(|note| note.pitch)
                    .collect();
                others.sort_unstable();
                pitch_classes(others)
            });
            if classes.is_empty() || classes.contains(&pitch_class_name(row.pitch)) {
                continue;
            }
            findings.push(format!(
                "{} at {} on {}: sounding {}",
                note_name(row.pitch),
                format_position(tick, meter),
                part.track,
                classes.join(" ")
            ));
        }
    }
    findings
}

/// One line per pair of written notes of one pitch that overlap: the write
/// cuts the earlier where the later starts, and of two starting together
/// keeps one.
fn overlaps(parts: &[Part], meter: Meter) -> Vec<String> {
    let mut findings = Vec::new();
    for part in parts {
        let mut rows = part.rows.clone();
        rows.sort_by_key(|row| (row.pitch, row.start));
        let track = &part.track;
        for pair in rows.windows(2) {
            let (earlier, later) = (pair[0], pair[1]);
            if earlier.pitch != later.pitch || later.start >= earlier.start + earlier.length {
                continue;
            }
            let name = note_name(later.pitch);
            let at = format_position(part.start + later.start, meter);
            findings.push(if later.start == earlier.start {
                format!("Two {name} start at {at} on {track}: the write keeps one.")
            } else {
                format!(
                    "{name} at {at} on {track} starts while the {name} at {} still sounds: \
                     the write cuts the earlier one there.",
                    format_position(part.start + earlier.start, meter)
                )
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::models::clip::Clip;
    use crate::models::event::Event;

    use super::*;

    /// One bar of 4/4.
    const BAR: i32 = 3840;

    /// A clip at `start` of `length` ticks holding `(pitch, start, length)`
    /// notes, in ticks from the clip's start.
    fn clip(start: i32, length: i32, notes: &[(u8, i32, i32)]) -> Clip {
        let mut clip = clip_at(start, length);
        for &(pitch, at, len) in notes {
            clip.add_event(Event::new(at, 0, vec![0x90, pitch, 100]));
            clip.add_event(Event::new(at + len, 0, vec![0x80, pitch, 0]));
        }
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip
    }

    /// No loop region, track 0 `bass`, track 1 `keys` holding an A minor
    /// chord over bar 1 and a G major one over bar 2.
    fn sequencer() -> Sequencer {
        let mut sequencer = test_sequencer();
        sequencer.set_global_region(0, 0);
        sequencer.tracks_mut()[0].set_name(Some("bass".into()));
        sequencer.tracks_mut()[1].set_name(Some("keys".into()));
        let keys = clip(
            0,
            2 * BAR,
            &[
                (57, 0, BAR),
                (60, 0, BAR),
                (64, 0, BAR),
                (55, BAR, BAR),
                (59, BAR, BAR),
                (62, BAR, BAR),
            ],
        );
        assert!(sequencer.tracks_mut()[1].add_clip(&keys));
        sequencer
    }

    /// `check_notes` of one bass clip over bars 1–2 holding `notes`.
    fn check_bass(sequencer: &Sequencer, notes: &str) -> String {
        let args = json!({
            "clips": [{ "track": "bass", "start": "1", "length": 8, "notes": notes }],
            "humanize": false,
        });
        check_notes(sequencer, &args).unwrap()
    }

    /// The lines after `Findings:`.
    fn findings(text: &str) -> Vec<&str> {
        text.lines()
            .skip_while(|line| *line != "Findings:")
            .skip(1)
            .collect()
    }

    #[test]
    fn a_line_in_the_harmony_has_no_findings_and_writes_nothing() {
        let sequencer = sequencer();
        let text = check_bass(&sequencer, "A2 0 2 90\nE2 2 2 90\nG2 4 4 90");
        assert!(text.starts_with("Dry run, nothing written."), "{text}");
        assert!(
            text.contains("Clip 1, track 1 \"bass\", 1 → 3:\nA2 0 2 90"),
            "{text}"
        );
        assert!(text.ends_with("No findings."), "{text}");
        assert!(sequencer.tracks()[0].clips().is_empty());
    }

    #[test]
    fn a_note_outside_the_other_tracks_pitch_classes_is_named_at_its_beat() {
        let sequencer = sequencer();
        // F# over A minor; G over G major is fine.
        let text = check_bass(&sequencer, "F#2 1.5 0.5 90\nG2 4 4 90");
        assert_eq!(
            findings(&text),
            vec!["F#2 at 1+1.5 on track 1 \"bass\": sounding A C E"],
            "{text}"
        );
    }

    #[test]
    fn several_new_clips_are_checked_against_each_other() {
        let sequencer = sequencer();
        // Bars 3–4 are empty: the new keys part is the only harmony there.
        let args = json!({
            "clips": [
                { "track": 1, "start": "3", "length": 4, "notes": "C#2 0 4 90" },
                { "track": 3, "start": "3", "length": 4, "notes": "D4 0 4 80\nF#4 0 4 80" },
            ],
            "humanize": false,
        });
        let text = check_notes(&sequencer, &args).unwrap();
        let found = findings(&text);
        assert!(
            found.contains(&"C#2 at 3 on track 1 \"bass\": sounding D F#"),
            "{text}"
        );
    }

    #[test]
    fn the_cleanup_overlaps_and_a_clip_in_the_way_are_findings() {
        let mut sequencer = sequencer();
        assert!(sequencer.tracks_mut()[0].add_clip(&clip(BAR, BAR, &[])));
        let text = check_bass(&sequencer, "A2 0.1 2 90\nA2 1 1 90\nE2 4 1 90\nE2 4 1 90");
        let found = findings(&text);
        assert!(
            found.contains(&"Cleanup: 1 note moved to the 1/16 grid."),
            "{text}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.starts_with("A2 at 1+1 on track 1 \"bass\" starts while")),
            "{text}"
        );
        assert!(
            found.contains(&"Two E2 start at 2 on track 1 \"bass\": the write keeps one."),
            "{text}"
        );
        assert!(
            found.contains(
                &"track 1 \"bass\" has a clip at 2 → 3: create_clips refuses to write over \
                  it unless replace_existing."
            ),
            "{text}"
        );
    }

    #[test]
    fn a_replace_is_checked_without_the_notes_it_replaces() {
        let mut sequencer = sequencer();
        // The keys clip itself is replaced: only the bass is harmony.
        let bass = clip(0, BAR, &[(45, 0, BAR)]);
        assert!(sequencer.tracks_mut()[0].add_clip(&bass));
        let keys = &sequencer.tracks()[1].clips()[0];
        let id = keys.id().simple().to_string()[..8].to_owned();
        let args = json!({ "clip": id, "notes": "C4 0 1 80\nA#3 1 1 80", "humanize": false });
        let text = check_notes(&sequencer, &args).unwrap();
        assert!(
            text.contains(&format!("Clip {id}, track 2 \"keys\", 1 → 3:")),
            "{text}"
        );
        assert_eq!(
            findings(&text),
            vec![
                "C4 at 1 on track 2 \"keys\": sounding A",
                "A#3 at 1+1 on track 2 \"keys\": sounding A"
            ],
            "{text}"
        );
    }

    #[test]
    fn a_bad_call_is_refused_as_the_write_would_refuse_it() {
        let sequencer = sequencer();
        assert!(check_notes(&sequencer, &json!({})).is_err());
        let error = check_notes(
            &sequencer,
            &json!({ "clip": "deadbeef", "notes": "C3 0 1 90" }),
        )
        .unwrap_err();
        assert!(error.contains("Unknown clip"), "{error}");
    }
}
