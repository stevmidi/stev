//! The writing tools (`360-mcp-server.md` § Writing: `create_clips`,
//! `replace_clip_notes`, `set_tempo`) and the cleanup every written note
//! goes through (decisions 10 and 16). Each reads the [`Sequencer`] and
//! builds its edit without changing anything; the `"sequencer"` thread's
//! handler records it as one undo step and shows the footer
//! ([`AgentWrite`]). A write refuses, changing nothing, while a take
//! session, an audio export or a modal holds the edit (decision 13, checked
//! once in [`write()`]), over existing clips unless told (decision 22), and
//! past its caps (§ Security).

use std::borrow::Cow;

use serde_json::Value;
use uuid::Uuid;

use crate::core::sequencer::{
    CreateClipsEdit, ReplaceClipNotesEdit, Sequencer, SequencerEdit, SetTempoEdit,
};
use crate::core::time::{
    Meter, PPQN, TEMPO_BPM_MAX, TEMPO_BPM_MIN, bpm_tenths_to_tempo_us, clamp_bpm_tenths,
    format_bpm, sixteenth_straight_ticks,
};
use crate::core::xorshift::XorShift;
use crate::models::clip::{Clip, nearest_grid_tick, nearest_swung_grid_tick};
use crate::models::event::Event;

use super::reads::{
    NOTES_MAX_BARS, clips_in, loop_region, position_arg, range_text, short_id, track_index,
    track_title,
};
use super::units::{NoteRow, format_beats, parse_beats, parse_rows};
use super::{AgentAnswer, AgentReply, AgentWrite};

/// The most clips one `create_clips` call writes.
pub(super) const MAX_CLIPS: usize = 16;

/// The most notes one write carries, all its clips together.
pub(super) const MAX_NOTES: usize = 2048;

/// The most bars one written clip spans: what `get_notes` reads at a time.
pub(super) const MAX_CLIP_BARS: i32 = NOTES_MAX_BARS;

/// The shortest note the cleanup leaves: a 1/32.
const MIN_NOTE_TICKS: i32 = PPQN / 8;

/// How far the humanising moves a start, in ticks either way.
pub(super) const JITTER_TICKS: i32 = 6;

/// The grid sizes a write snaps to, as `1/N` of a whole note: straight
/// and triplet.
const GRID_DIVISIONS: [i32; 11] = [1, 2, 4, 8, 16, 32, 3, 6, 12, 24, 48];

/// Answers the write tool `tool` (`create_clips`, `replace_clip_notes` or
/// `set_tempo`), or says why it can't run now.
pub(super) fn write(
    tool: &str,
    sequencer: &Sequencer,
    args: &Value,
) -> Result<AgentAnswer, String> {
    if let Some(why) = write_hold(sequencer) {
        return Err(why);
    }
    let mut jitter = Jitter::seeded();
    match tool {
        "create_clips" => create_clips(sequencer, args, &mut jitter).map(AgentAnswer::from),
        "replace_clip_notes" => {
            replace_clip_notes(sequencer, args, &mut jitter).map(AgentAnswer::from)
        }
        _ => set_tempo(sequencer, args),
    }
}

/// Why a write can't run now, if something else holds the edit
/// (decision 13).
fn write_hold(sequencer: &Sequencer) -> Option<String> {
    let why = if sequencer.is_framing_take() {
        "Finish the take first: the user has a take open in Stev's clip view \
         (Enter inserts it, Esc cancels)."
    } else if sequencer.is_exporting_audio() {
        "Stev is exporting audio; try again when the export is done."
    } else if sequencer.is_modal_open() {
        "A dialog is open in Stev; ask the user to close it, then try again."
    } else {
        return None;
    };
    Some(format!("{why} Nothing was changed."))
}

/// The grid a write snaps starts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grid {
    /// No snapping: starts stay where they were written.
    Off,
    /// Every this many ticks from event tick 0, as `Q` quantizes; the 1/16
    /// swung by the clip's swing.
    Step(i32),
}

impl Grid {
    /// The `grid` argument: `"1/16"` (the default), `"1/8"`, a triplet
    /// `"1/12"` or `"1/24"`, or `"off"`.
    fn from_args(args: &Value) -> Result<Grid, String> {
        let text = match args.get("grid") {
            None | Some(Value::Null) => return Ok(Grid::Step(sixteenth_straight_ticks())),
            Some(Value::String(text)) => text.trim().to_ascii_lowercase(),
            Some(_) => String::new(),
        };
        if matches!(text.as_str(), "off" | "none") {
            return Ok(Grid::Off);
        }
        let division = text.strip_prefix("1/").and_then(|n| n.parse::<i32>().ok());
        match division.filter(|n| GRID_DIVISIONS.contains(n)) {
            Some(n) => Ok(Grid::Step(PPQN * 4 / n)),
            None => Err(
                "grid is \"1/16\" (the default), \"1/8\", \"1/4\", \"1/32\", a triplet \
                 \"1/12\" or \"1/24\", or \"off\"."
                    .to_owned(),
            ),
        }
    }

    /// The grid as the replies name it: `1/16`, `1/12`.
    fn label(self) -> String {
        match self {
            Grid::Off => "off".to_owned(),
            Grid::Step(step) => format!("1/{}", PPQN * 4 / step),
        }
    }
}

/// The humanising: a start moved by up to [`JITTER_TICKS`] either way,
/// seeded per write.
pub(super) struct Jitter(XorShift);

impl Jitter {
    /// A jitter seeded from a fresh random UUID.
    fn seeded() -> Jitter {
        Jitter::from_seed(Uuid::new_v4().as_u64_pair().0)
    }

    /// A jitter from `seed`; the same seed gives the same moves.
    fn from_seed(seed: u64) -> Jitter {
        Jitter(XorShift::new(seed))
    }

    /// The next move, `-JITTER_TICKS..=JITTER_TICKS`.
    fn next(&mut self) -> i32 {
        self.0.below((2 * JITTER_TICKS + 1) as u64) as i32 - JITTER_TICKS
    }
}

/// How one clip's notes are cleaned up (decision 10).
struct Cleanup {
    /// The clip's event tick that row start 0 lands on: its window start.
    origin: i32,
    /// The clip's length in ticks; a note is cut at `origin + length`.
    length: i32,
    /// What starts snap to.
    grid: Grid,
    /// The clip's swing, 50–75, bending the 1/16 grid (decision 16).
    swing: u8,
}

/// What the cleanup changed, counted for the reply so the model learns.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CleanupReport {
    /// Notes whose start was off the grid.
    snapped: usize,
    /// Notes cut at the clip's end.
    cut: usize,
    /// Notes lengthened to a 1/32.
    lengthened: usize,
    /// Pitches outside 0–127.
    pitch_clamped: usize,
    /// Velocities outside 1–127.
    velocity_clamped: usize,
    /// Notes starting before the clip or at or after its end, left out.
    left_out: usize,
}

impl CleanupReport {
    /// The report as one line: `Cleanup: 2 notes moved to the 1/16 grid,
    /// 1 note cut at the clip end. Starts humanised by up to 6 ticks.`
    pub(super) fn text(&self, grid: Grid, humanize: bool) -> String {
        let grid_label = grid.label();
        let parts: Vec<String> = [
            (self.snapped, format!("moved to the {grid_label} grid")),
            (self.cut, "cut at the clip end".to_owned()),
            (self.lengthened, "lengthened to a 1/32".to_owned()),
            (
                self.left_out,
                "left out (starting outside the clip)".to_owned(),
            ),
            (
                self.pitch_clamped,
                "with a pitch clamped to 0–127".to_owned(),
            ),
            (
                self.velocity_clamped,
                "with a velocity clamped to 1–127".to_owned(),
            ),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{} {what}", counted(count, "note")))
        .collect();
        let mut text = if parts.is_empty() {
            "Cleanup: nothing to change.".to_owned()
        } else {
            format!("Cleanup: {}.", parts.join(", "))
        };
        if humanize {
            text.push_str(&format!(" Starts humanised by up to {JITTER_TICKS} ticks."));
        }
        text
    }
}

/// `1 note`, `3 clips`.
pub(super) fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Refuses a write of more than [`MAX_NOTES`] notes.
fn check_note_count(count: usize) -> Result<(), String> {
    if count > MAX_NOTES {
        return Err(format!(
            "More than {MAX_NOTES} notes in one call; write fewer at a time."
        ));
    }
    Ok(())
}

impl Cleanup {
    /// `rows` (starts in ticks from the clip's start) cleaned up: starts on
    /// the grid, humanised with `jitter`, pitches and velocities clamped,
    /// lengths at least a 1/32 and cut at the clip end, what changed added
    /// to `report`. The rows come back in the clip's event ticks; rows
    /// starting outside the clip are left out, and a write with none left
    /// is refused.
    fn clean(
        &self,
        rows: &[NoteRow],
        mut jitter: Option<&mut Jitter>,
        report: &mut CleanupReport,
    ) -> Result<Vec<NoteRow>, String> {
        let end = self.origin + self.length;
        let mut cleaned = Vec::with_capacity(rows.len());
        for row in rows {
            if row.start < 0 || row.start >= self.length {
                report.left_out += 1;
                continue;
            }
            let written = self.origin + row.start;
            let mut start = match self.grid {
                Grid::Off => written,
                Grid::Step(step) => {
                    let straight = nearest_grid_tick(written, step);
                    report.snapped += usize::from(straight != written);
                    if step == sixteenth_straight_ticks() {
                        nearest_swung_grid_tick(written, self.swing)
                    } else {
                        straight
                    }
                }
            };
            // Snapped onto the clip's end: stays where it was written.
            if start >= end {
                start = written;
            }
            if let Some(jitter) = jitter.as_deref_mut() {
                start += jitter.next();
            }
            let start = start.clamp(self.origin, end - 1);

            let mut length = row.length;
            if length < MIN_NOTE_TICKS {
                report.lengthened += 1;
                length = MIN_NOTE_TICKS;
            }
            if start + length > end {
                report.cut += 1;
                length = end - start;
            }
            let pitch = row.pitch.clamp(0, 127);
            report.pitch_clamped += usize::from(pitch != row.pitch);
            let velocity = row.velocity.clamp(1, 127);
            report.velocity_clamped += usize::from(velocity != row.velocity);
            cleaned.push(NoteRow {
                pitch,
                start,
                length,
                velocity,
            });
        }
        if cleaned.is_empty() {
            return Err(format!(
                "every note starts outside the clip (0 to {} beats); nothing was changed.",
                format_beats(self.length)
            ));
        }
        Ok(cleaned)
    }
}

/// `clip`'s notes from cleaned `rows` (event ticks), as `(note_on,
/// note_off)` pairs.
fn note_pairs(clip: &Clip, rows: &[NoteRow]) -> Vec<(Event, Event)> {
    rows.iter()
        .map(|row| clip.new_note(row.start, row.length, row.pitch, row.velocity))
        .collect()
}

/// The `notes` argument of `args` as rows: one string of lines (or a list
/// of row strings).
fn notes_arg(args: &Value, what: &str) -> Result<Vec<NoteRow>, String> {
    let text: Cow<str> = match args.get("notes") {
        Some(Value::String(text)) => Cow::Borrowed(text),
        Some(Value::Array(lines)) => Cow::Owned(
            lines
                .iter()
                .map(|line| line.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => {
            return Err(format!(
                "{what} needs notes: rows \"pitch start length velocity\", one per line."
            ));
        }
    };
    let rows = parse_rows(&text).map_err(|e| format!("{what} notes, {e}"))?;
    if rows.is_empty() {
        return Err(format!("{what} has no notes."));
    }
    Ok(rows)
}

/// A boolean argument, `default` when absent.
fn bool_arg(args: &Value, name: &str, default: bool) -> Result<bool, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("{name} is true or false.")),
    }
}

/// A length in beats (`16`, `4.5`, or the same as a string) as ticks.
fn beats_value(value: Option<&Value>) -> Option<i32> {
    match value? {
        Value::Number(number) => parse_beats(&number.to_string()),
        Value::String(text) => parse_beats(text),
        _ => None,
    }
}

/// The swing a new clip over `[start, end)` on `track_idx` takes when the
/// call names none (decision 16): the clip sounding longest over the same
/// span on its own track, else the part it continues there (the nearest
/// clip ending at or before `start`), else the longest-overlapping clip on
/// another track; straight (50) when there is none.
fn default_swing(sequencer: &Sequencer, track_idx: usize, start: i32, end: i32) -> u8 {
    // The longest-overlapping clip on a track, with how much it overlaps.
    let longest_over = |idx: usize| {
        clips_in(&sequencer.tracks()[idx], start, end)
            .map(|clip| {
                (
                    clip.end_tick().min(end) - clip.start_tick().max(start),
                    clip,
                )
            })
            .max_by_key(|(overlap, _)| *overlap)
    };
    longest_over(track_idx)
        .map(|(_, clip)| clip)
        .or_else(|| {
            sequencer.tracks()[track_idx]
                .clips()
                .iter()
                .filter(|clip| clip.end_tick() <= start)
                .max_by_key(|clip| clip.end_tick())
        })
        .or_else(|| {
            (0..sequencer.tracks().len())
                .filter(|&idx| idx != track_idx)
                .filter_map(longest_over)
                .max_by_key(|(overlap, _)| *overlap)
                .map(|(_, clip)| clip)
        })
        .map_or(50, Clip::swing_pct)
}

/// The bars `[start, end)` covers, for the footer: `bar 17`, `bars 17–20`.
fn bars_text(start: i32, end: i32, meter: Meter) -> String {
    let first = start.div_euclid(meter.bar_ticks()) + 1;
    let last = (end - 1).div_euclid(meter.bar_ticks()) + 1;
    if first == last {
        format!("bar {first}")
    } else {
        format!("bars {first}–{last}")
    }
}

/// One clip of a `create_clips` call, read and checked.
pub(super) struct NewClip {
    /// The track it goes on.
    pub(super) track_idx: usize,
    /// The clip, notes cleaned up, ready to paste.
    pub(super) clip: Clip,
    /// The cleaned rows, in the clip's event ticks, before same-pitch
    /// overlaps are trimmed.
    pub(super) rows: Vec<NoteRow>,
}

/// A `create_clips` call read, cleaned up and checked, nothing built yet:
/// what the write records and `check_notes` reports.
pub(super) struct PreparedClips {
    /// The clips, in the call's order.
    pub(super) clips: Vec<NewClip>,
    /// What the cleanup changed.
    pub(super) report: CleanupReport,
    /// What starts snapped to.
    pub(super) grid: Grid,
    /// Whether starts are humanised.
    pub(super) humanize: bool,
    /// The existing clips the new ones land on, as the replies name them:
    /// `track 2 has a clip at 17 → 25` (decision 22).
    pub(super) in_the_way: Vec<String>,
}

/// Reads, cleans up and checks a `create_clips` call, humanising with
/// `jitter` when the call asks for it. Refuses a bad or oversized call,
/// not one that lands on existing clips: that is [`PreparedClips::in_the_way`].
pub(super) fn prepare_clips(
    sequencer: &Sequencer,
    args: &Value,
    mut jitter: Option<&mut Jitter>,
) -> Result<PreparedClips, String> {
    let meter = sequencer.meter();
    let grid = Grid::from_args(args)?;
    let humanize = bool_arg(args, "humanize", true)?;
    let specs = match args.get("clips") {
        Some(Value::Array(specs)) if !specs.is_empty() => specs,
        _ => {
            return Err(
                "create_clips needs clips: a list of { track, start, length, notes }.".to_owned(),
            );
        }
    };
    if specs.len() > MAX_CLIPS {
        return Err(format!(
            "{} clips in one call; create_clips writes at most {MAX_CLIPS}.",
            specs.len()
        ));
    }

    let mut report = CleanupReport::default();
    let mut new_clips: Vec<NewClip> = Vec::with_capacity(specs.len());
    let mut note_count = 0;
    for (i, spec) in specs.iter().enumerate() {
        let what = format!("Clip {}", i + 1);
        let track_idx = match spec.get("track") {
            Some(track) => track_index(sequencer, track).map_err(|e| format!("{what}: {e}"))?,
            None => return Err(format!("{what} needs a track.")),
        };
        let start = position_arg(spec, "start", meter)
            .map_err(|e| format!("{what}: {e}"))?
            .ok_or_else(|| format!("{what} needs a start, a position like \"17\"."))?;
        let length = beats_value(spec.get("length"))
            .filter(|&length| length > 0)
            .ok_or_else(|| format!("{what} needs a length in beats, more than 0."))?;
        if length > MAX_CLIP_BARS * meter.bar_ticks() {
            return Err(format!(
                "{what} is {} beats long; a written clip is at most {MAX_CLIP_BARS} bars.",
                format_beats(length)
            ));
        }
        let rows = notes_arg(spec, &what)?;
        note_count += rows.len();
        check_note_count(note_count)?;
        let swing = match spec.get("swing") {
            None | Some(Value::Null) => default_swing(sequencer, track_idx, start, start + length),
            Some(value) => value
                .as_u64()
                .filter(|swing| (50..=75).contains(swing))
                .map(|swing| swing as u8)
                .ok_or_else(|| format!("{what}: swing is a number from 50 (straight) to 75."))?,
        };

        let mut clip = Clip::new();
        clip.region_mut().set_region(Some(0), Some(length));
        clip.set_start_tick(start);
        clip.set_swing_pct(swing);
        let cleanup = Cleanup {
            origin: 0,
            length,
            grid,
            swing,
        };
        let rows = cleanup
            .clean(
                &rows,
                jitter.as_deref_mut().filter(|_| humanize),
                &mut report,
            )
            .map_err(|e| format!("{what}: {e}"))?;
        clip.insert_notes(note_pairs(&clip, &rows));

        let end = start + length;
        if let Some(other) = new_clips.iter().position(|other| {
            other.track_idx == track_idx
                && other.clip.start_tick() < end
                && other.clip.end_tick() > start
        }) {
            return Err(format!(
                "Clips {} and {} overlap on {}; nothing was changed.",
                other + 1,
                i + 1,
                track_title(track_idx, &sequencer.tracks()[track_idx])
            ));
        }
        new_clips.push(NewClip {
            track_idx,
            clip,
            rows,
        });
    }

    // Decision 22: what the clips would land on.
    let in_the_way = new_clips
        .iter()
        .flat_map(|new| {
            let track = &sequencer.tracks()[new.track_idx];
            clips_in(track, new.clip.start_tick(), new.clip.end_tick()).map(move |clip| {
                format!(
                    "{} has a clip at {}",
                    track_title(new.track_idx, track),
                    range_text(clip.start_tick(), clip.end_tick(), meter)
                )
            })
        })
        .collect();
    Ok(PreparedClips {
        clips: new_clips,
        report,
        grid,
        humanize,
        in_the_way,
    })
}

/// `create_clips`: new clips with notes, one undo step for all of them.
fn create_clips(
    sequencer: &Sequencer,
    args: &Value,
    jitter: &mut Jitter,
) -> Result<AgentWrite, String> {
    let meter = sequencer.meter();
    let replace_existing = bool_arg(args, "replace_existing", false)?;
    let extend_loop = bool_arg(args, "extend_loop", false)?;
    let PreparedClips {
        clips: new_clips,
        report,
        grid,
        humanize,
        in_the_way,
    } = prepare_clips(sequencer, args, Some(jitter))?;
    if !in_the_way.is_empty() && !replace_existing {
        return Err(format!(
            "{}. Nothing was changed: ask the user before writing over it \
             (replace_existing: true carves it out), or write somewhere free.",
            in_the_way.join("; ")
        ));
    }

    let mut lines = vec![format!(
        "Wrote {} as one undo step:",
        counted(new_clips.len(), "clip")
    )];
    for new in &new_clips {
        lines.push(format!(
            "{}: clip {}, {}",
            track_title(new.track_idx, &sequencer.tracks()[new.track_idx]),
            new_clip_summary(&new.clip, meter),
            counted(new.rows.len(), "note")
        ));
    }
    if !in_the_way.is_empty() {
        lines.push(format!(
            "Carved out what was there: {}.",
            in_the_way.join("; ")
        ));
    }
    lines.push(report.text(grid, humanize));

    let first = new_clips
        .iter()
        .map(|new| new.clip.start_tick())
        .min()
        .unwrap_or(0);
    let last = new_clips
        .iter()
        .map(|new| new.clip.end_tick())
        .max()
        .unwrap_or(0);
    let count = new_clips.len();
    let targets = new_clips
        .into_iter()
        .map(|new| (new.track_idx, new.clip))
        .collect();
    let edit = CreateClipsEdit::new(sequencer, targets, extend_loop)
        .ok_or_else(|| "Those tracks aren't there; nothing was changed.".to_owned())?;
    lines.push(loop_note(sequencer, &edit, first, last, extend_loop));

    Ok(AgentWrite {
        edit: edit.into(),
        reply: lines.join("\n"),
        footer: format!(
            "Agent: {}, {}",
            counted(count, "new clip"),
            bars_text(first, last, meter)
        ),
    })
}

/// A new clip as the replies name it: `a3f0c2e1 17 → 21, swing 58`.
pub(super) fn new_clip_summary(clip: &Clip, meter: Meter) -> String {
    let swing = if clip.swing_pct() > 50 {
        format!(", swing {}", clip.swing_pct())
    } else {
        String::new()
    };
    format!(
        "{} {}{swing}",
        short_id(clip),
        range_text(clip.start_tick(), clip.end_tick(), meter)
    )
}

/// Where the new clips `[first, last)` sit against the loop region, so the
/// agent can tell the user whether the loop plays them (decision 22).
fn loop_note(
    sequencer: &Sequencer,
    edit: &CreateClipsEdit,
    first: i32,
    last: i32,
    extend_loop: bool,
) -> String {
    let meter = sequencer.meter();
    let looping = if sequencer.is_loop_enabled() {
        "on"
    } else {
        "off"
    };
    if let Some((start, end)) = edit.new_region() {
        return format!(
            "The loop region now runs {} (looping {looping}).",
            range_text(start, end, meter)
        );
    }
    let Some((start, end)) = loop_region(sequencer) else {
        return if extend_loop {
            "There is no loop region to extend.".to_owned()
        } else {
            "There is no loop region.".to_owned()
        };
    };
    let region = range_text(start, end, meter);
    if first >= start && last <= end {
        format!("They're inside the loop region {region} (looping {looping}).")
    } else if last <= start || first >= end {
        format!(
            "They're outside the loop region {region} (looping {looping}): the loop doesn't \
             play them until the user moves it, or you write again with extend_loop."
        )
    } else {
        format!("They're partly outside the loop region {region} (looping {looping}).")
    }
}

/// The clip a `clip` argument names: its short id (decision 12) or any
/// longer prefix of its UUID, hyphens or not.
fn find_clip(sequencer: &Sequencer, args: &Value) -> Result<(usize, Uuid), String> {
    let Some(text) = args.get("clip").and_then(Value::as_str) else {
        return Err("replace_clip_notes needs clip: the id get_context gives.".to_owned());
    };
    let wanted: String = text
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let unknown = || format!("Unknown clip {}; call get_context again.", text.trim());
    if wanted.len() < 8 || !wanted.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(unknown());
    }
    let mut found = sequencer
        .tracks()
        .iter()
        .enumerate()
        .flat_map(|(idx, track)| {
            track
                .clips()
                .iter()
                .filter(|clip| {
                    clip.id()
                        .simple()
                        .encode_lower(&mut Uuid::encode_buffer())
                        .starts_with(&wanted)
                })
                .map(move |clip| (idx, clip.id()))
        });
    match (found.next(), found.next()) {
        (Some(clip), None) => Ok(clip),
        (Some(_), Some(_)) => Err(format!(
            "Two clips start with {}; pass more of the id.",
            text.trim()
        )),
        (None, _) => Err(unknown()),
    }
}

/// A `replace_clip_notes` call read and cleaned up, nothing built yet:
/// what the write records and `check_notes` reports.
pub(super) struct PreparedReplace {
    /// The clip's track.
    pub(super) track_idx: usize,
    /// The clip.
    pub(super) clip_id: Uuid,
    /// The cleaned rows, in the clip's event ticks.
    pub(super) rows: Vec<NoteRow>,
    /// What the cleanup changed.
    pub(super) report: CleanupReport,
    /// What starts snapped to.
    pub(super) grid: Grid,
    /// Whether starts are humanised.
    pub(super) humanize: bool,
}

/// What a `replace_clip_notes` call names when its clip is gone.
fn clip_gone() -> String {
    "That clip is gone; call get_context again.".to_owned()
}

/// Reads and cleans up a `replace_clip_notes` call, humanising with
/// `jitter` when the call asks for it.
pub(super) fn prepare_replace(
    sequencer: &Sequencer,
    args: &Value,
    jitter: Option<&mut Jitter>,
) -> Result<PreparedReplace, String> {
    let (track_idx, clip_id) = find_clip(sequencer, args)?;
    let grid = Grid::from_args(args)?;
    let humanize = bool_arg(args, "humanize", true)?;
    let rows = notes_arg(args, "replace_clip_notes")?;
    check_note_count(rows.len())?;
    let clip = sequencer
        .clip_on(track_idx, clip_id)
        .ok_or_else(clip_gone)?;
    let cleanup = Cleanup {
        origin: clip.region().start(),
        length: clip.region_length(),
        grid,
        swing: clip.swing_pct(),
    };
    let mut report = CleanupReport::default();
    let rows = cleanup
        .clean(&rows, jitter.filter(|_| humanize), &mut report)
        .map_err(|e| format!("replace_clip_notes: {e}"))?;
    Ok(PreparedReplace {
        track_idx,
        clip_id,
        rows,
        report,
        grid,
        humanize,
    })
}

/// `replace_clip_notes`: every note of one clip replaced, its position,
/// window and length kept.
fn replace_clip_notes(
    sequencer: &Sequencer,
    args: &Value,
    jitter: &mut Jitter,
) -> Result<AgentWrite, String> {
    let meter = sequencer.meter();
    let clear_wheels = bool_arg(args, "clear_wheels", false)?;
    let PreparedReplace {
        track_idx,
        clip_id,
        rows,
        report,
        grid,
        humanize,
    } = prepare_replace(sequencer, args, Some(jitter))?;
    let track = &sequencer.tracks()[track_idx];
    let clip = sequencer
        .clip_on(track_idx, clip_id)
        .ok_or_else(clip_gone)?;
    let pairs = note_pairs(clip, &rows);
    let count = pairs.len();
    let range = range_text(clip.start_tick(), clip.end_tick(), meter);
    let bars = bars_text(clip.start_tick(), clip.end_tick(), meter);
    let edit = ReplaceClipNotesEdit::new(sequencer, track_idx, clip_id, pairs, clear_wheels)
        .ok_or_else(clip_gone)?;
    let reply = format!(
        "Replaced the notes of clip {} ({}, {range}) with {}, wheels {}, as one undo step.\n{}",
        short_id(clip),
        track_title(track_idx, track),
        counted(count, "note"),
        if clear_wheels { "cleared" } else { "kept" },
        report.text(grid, humanize)
    );
    Ok(AgentWrite {
        edit: edit.into(),
        reply,
        footer: format!("Agent: new notes on track {}, {bars}", track_idx + 1),
    })
}

/// `set_tempo`: the project tempo, clamped to the app's range. Just an
/// answer, nothing to record, when it is that tempo already.
fn set_tempo(sequencer: &Sequencer, args: &Value) -> Result<AgentAnswer, String> {
    let bpm = match args.get("bpm") {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
    .ok_or_else(|| "set_tempo needs bpm, a number like 96 or 92.5.".to_owned())?;
    let tenths = (bpm * 10.0).round().min(f64::from(i32::MAX)) as i32;
    let clamped = clamp_bpm_tenths(tenths);
    let tempo_us = bpm_tenths_to_tempo_us(clamped);
    let shown = format_bpm(tempo_us);
    let Some(edit) = SetTempoEdit::new(sequencer, tempo_us, None) else {
        return Ok(AgentAnswer::Reply(AgentReply::ok(format!(
            "The tempo is already {shown} BPM; nothing changed."
        ))));
    };
    let mut reply = format!("Tempo set to {shown} BPM, as one undo step.");
    if clamped != tenths {
        reply.push_str(&format!(
            " Stev's tempos run {TEMPO_BPM_MIN}–{TEMPO_BPM_MAX} BPM, so {bpm} was clamped."
        ));
    }
    Ok(AgentWrite {
        edit: SequencerEdit::from(edit),
        reply,
        footer: format!("Agent: tempo {shown} BPM"),
    }
    .into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use undo::Record;

    use crate::core::sequencer::test_support::{clip_at, test_sequencer};
    use crate::models::event::EventType;

    use super::*;

    /// One bar of 4/4.
    const BAR: i32 = 3840;

    /// A note row, in ticks.
    fn row(pitch: i32, start: i32, length: i32, velocity: i32) -> NoteRow {
        NoteRow {
            pitch,
            start,
            length,
            velocity,
        }
    }

    /// A one-bar cleanup at event tick 0 on `grid` and `swing`.
    fn cleanup(grid: Grid, swing: u8) -> Cleanup {
        Cleanup {
            origin: 0,
            length: BAR,
            grid,
            swing,
        }
    }

    /// `rows` cleaned up by `cleanup`, with what changed.
    fn clean(
        cleanup: &Cleanup,
        rows: &[NoteRow],
        jitter: Option<&mut Jitter>,
    ) -> (Vec<NoteRow>, CleanupReport) {
        let mut report = CleanupReport::default();
        let rows = cleanup.clean(rows, jitter, &mut report).unwrap();
        (rows, report)
    }

    /// The starts `rows` come out at, unhumanised.
    fn starts(cleanup: &Cleanup, rows: &[NoteRow]) -> Vec<i32> {
        let (rows, _) = clean(cleanup, rows, None);
        rows.iter().map(|row| row.start).collect()
    }

    /// Fails unless `text` says `needle`, showing `text`.
    #[track_caller]
    fn assert_says(text: &str, needle: &str) {
        assert!(text.contains(needle), "{needle:?} not in {text:?}");
    }

    /// `create_clips` with a fixed jitter.
    fn create(sequencer: &Sequencer, args: &Value) -> Result<AgentWrite, String> {
        create_clips(sequencer, args, &mut Jitter::from_seed(1))
    }

    /// The test sequencer with no loop region, track 0 named `bass`.
    fn sequencer() -> Sequencer {
        let mut sequencer = test_sequencer();
        sequencer.set_global_region(0, 0);
        sequencer.tracks_mut()[0].set_name(Some("bass".into()));
        sequencer
    }

    /// A clip at `start` of `length` ticks swung by `swing`.
    fn swung_clip(start: i32, length: i32, swing: u8) -> Clip {
        let mut clip = clip_at(start, length);
        clip.set_swing_pct(swing);
        clip
    }

    /// `(pitch, start, length)` of every note `clip` holds, in event ticks.
    fn notes_of(clip: &Clip) -> Vec<(u8, i32, i32)> {
        clip.events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(|event| {
                (
                    event.note_number().unwrap(),
                    event.tick(),
                    event.end_tick() - event.tick(),
                )
            })
            .collect()
    }

    #[test]
    fn the_grid_argument_reads_straight_triplet_and_off() {
        let grid = |value| Grid::from_args(&json!({ "grid": value }));
        assert_eq!(Grid::from_args(&json!({})), Ok(Grid::Step(240)));
        assert_eq!(grid("1/8"), Ok(Grid::Step(480)));
        assert_eq!(grid("1/12"), Ok(Grid::Step(320)));
        assert_eq!(grid("off"), Ok(Grid::Off));
        assert!(grid("1/7").is_err());
        assert_eq!(Grid::Step(320).label(), "1/12");
    }

    #[test]
    fn starts_snap_to_the_straight_grid_and_the_report_counts_them() {
        let straight = cleanup(Grid::Step(240), 50);
        let rows = [row(60, 250, 240, 90), row(62, 480, 240, 90)];
        assert_eq!(starts(&straight, &rows), vec![240, 480]);
        let (_, report) = clean(&straight, &rows, None);
        assert_eq!(report.snapped, 1);
        assert_eq!(starts(&cleanup(Grid::Off, 50), &rows), vec![250, 480]);
    }

    #[test]
    fn the_sixteenth_grid_swings_with_the_clip_and_a_straight_off_beat_is_not_a_move() {
        // Swing 60 moves the off-beat 16ths 48 ticks late.
        let swung = cleanup(Grid::Step(240), 60);
        let rows = [
            row(60, 240, 120, 90),
            row(60, 480, 120, 90),
            row(60, 720, 120, 90),
        ];
        assert_eq!(starts(&swung, &rows), vec![288, 480, 768]);
        assert_eq!(clean(&swung, &rows, None).1.snapped, 0);
    }

    #[test]
    fn a_triplet_or_a_coarser_grid_ignores_the_swing() {
        let rows = [row(60, 330, 120, 90)];
        assert_eq!(starts(&cleanup(Grid::Step(320), 60), &rows), vec![320]);
        let rows = [row(60, 470, 120, 90)];
        assert_eq!(starts(&cleanup(Grid::Step(480), 60), &rows), vec![480]);
    }

    #[test]
    fn pitch_velocity_and_length_are_clamped_and_notes_cut_at_the_end() {
        let rule = cleanup(Grid::Step(240), 50);
        let rows = [
            row(130, 0, 960, 0),
            row(-2, 960, 10, 200),
            row(60, BAR - 480, 960, 90),
            row(60, -240, 480, 90),
            row(60, BAR, 480, 90),
        ];
        let (cleaned, report) = clean(&rule, &rows, None);
        assert_eq!(
            cleaned,
            vec![
                row(127, 0, 960, 1),
                row(0, 960, MIN_NOTE_TICKS, 127),
                row(60, BAR - 480, 480, 90),
            ]
        );
        assert_eq!(
            report,
            CleanupReport {
                snapped: 0,
                cut: 1,
                lengthened: 1,
                pitch_clamped: 2,
                velocity_clamped: 2,
                left_out: 2,
            }
        );
        let text = report.text(Grid::Step(240), false);
        assert_says(&text, "1 note cut at the clip end");
        assert_says(&text, "2 notes left out");
        assert!(!text.contains("humanised"), "{text}");
    }

    #[test]
    fn a_note_snapping_onto_the_end_stays_where_it_was_written() {
        let rule = cleanup(Grid::Step(240), 50);
        let (cleaned, _) = clean(&rule, &[row(60, BAR - 10, 480, 90)], None);
        assert_eq!(cleaned, vec![row(60, BAR - 10, 10, 90)]);
    }

    #[test]
    fn the_humanising_stays_within_its_ticks_and_inside_the_clip() {
        let rule = cleanup(Grid::Step(240), 50);
        let rows: Vec<NoteRow> = (0..200).map(|_| row(60, 960, 240, 90)).collect();
        let mut jitter = Jitter::from_seed(7);
        let (cleaned, _) = clean(&rule, &rows, Some(&mut jitter));
        let moves: Vec<i32> = cleaned.iter().map(|row| row.start - 960).collect();
        assert!(moves.iter().all(|m| m.abs() <= JITTER_TICKS), "{moves:?}");
        assert!(moves.iter().any(|&m| m != 0));
        assert!(moves.iter().any(|&m| m < 0) && moves.iter().any(|&m| m > 0));

        // A note on the clip's first tick never moves before it.
        let (cleaned, _) = clean(&rule, &[row(60, 0, 240, 90); 50], Some(&mut jitter));
        assert!(cleaned.iter().all(|row| row.start >= 0));
    }

    #[test]
    fn the_default_swing_is_the_tracks_own_then_what_it_continues_then_another_tracks() {
        let mut sequencer = sequencer();
        sequencer.tracks_mut()[1].add_clip(&swung_clip(BAR * 4, BAR * 4, 66));
        // Nothing on track 0: track 1's clip over the same bars.
        assert_eq!(default_swing(&sequencer, 0, BAR * 4, BAR * 8), 66);
        // Nothing anywhere over the bars: straight.
        assert_eq!(default_swing(&sequencer, 0, BAR * 20, BAR * 24), 50);

        // Track 0's own part before the span: the part it continues.
        sequencer.tracks_mut()[0].add_clip(&swung_clip(0, BAR * 4, 58));
        assert_eq!(default_swing(&sequencer, 0, BAR * 4, BAR * 8), 58);

        // Track 0's own clip over the span wins.
        sequencer.tracks_mut()[0].add_clip(&swung_clip(BAR * 6, BAR * 2, 54));
        assert_eq!(default_swing(&sequencer, 0, BAR * 4, BAR * 8), 54);
    }

    /// `create_clips` of one clip on track 1 (the first) at `start`.
    fn one_clip(start: &str, notes: &str) -> Value {
        json!({
            "clips": [{ "track": 1, "start": start, "length": 4, "notes": notes }],
            "humanize": false,
        })
    }

    #[test]
    fn create_clips_writes_snapped_clips_and_says_where() {
        let mut sequencer = sequencer();
        let mut record = Record::new();
        let args = json!({
            "clips": [
                { "track": "bass", "start": "2", "length": 4, "notes": "A2 0 1 96\nE2 1.02 1 90" },
                { "track": 2, "start": "2", "length": 8, "notes": "C4 0 2 80", "swing": 62 },
            ],
            "humanize": false,
        });
        let write = create(&sequencer, &args).unwrap();
        assert_eq!(write.footer, "Agent: 2 new clips, bars 2–3");
        assert_says(&write.reply, "track 1 \"bass\": clip ");
        assert_says(&write.reply, "2 → 3, 2 notes");
        assert_says(&write.reply, "swing 62");
        assert_says(&write.reply, "1 note moved to the 1/16 grid");
        assert_says(&write.reply, "There is no loop region");
        record.edit(&mut sequencer, write.edit);

        let bass = &sequencer.tracks()[0].clips()[0];
        assert_eq!((bass.start_tick(), bass.end_tick()), (BAR, BAR * 2));
        assert_eq!(notes_of(bass), vec![(45, 0, 960), (40, 960, 960)]);
        assert_eq!(sequencer.tracks()[1].clips()[0].swing_pct(), 62);

        record.undo(&mut sequencer);
        assert!(sequencer.tracks()[0].clips().is_empty());
        assert!(sequencer.tracks()[1].clips().is_empty());
    }

    #[test]
    fn create_clips_refuses_to_land_on_a_clip_unless_told_and_then_carves() {
        let mut sequencer = sequencer();
        sequencer.tracks_mut()[0].add_clip(&clip_at(0, BAR * 4));
        let args = one_clip("2", "C3 0 1 90");
        let error = create(&sequencer, &args).err().unwrap();
        assert_says(&error, "track 1 \"bass\" has a clip at 1 → 5");
        assert_says(&error, "Nothing was changed");

        let mut args = args;
        args["replace_existing"] = json!(true);
        let write = create(&sequencer, &args).unwrap();
        assert_says(&write.reply, "Carved out");
        let mut record = Record::new();
        record.edit(&mut sequencer, write.edit);
        let spans: Vec<(i32, i32)> = sequencer.tracks()[0]
            .clips()
            .iter()
            .map(|clip| (clip.start_tick(), clip.end_tick()))
            .collect();
        assert_eq!(spans, vec![(0, BAR), (BAR, BAR * 2), (BAR * 2, BAR * 4)]);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.tracks()[0].clips().len(), 1);
    }

    #[test]
    fn create_clips_refuses_bad_or_oversized_calls() {
        let sequencer = sequencer();
        let refuse = |args: Value| create(&sequencer, &args).err().unwrap();
        assert!(refuse(json!({})).contains("needs clips"));
        assert!(refuse(one_clip("2", "")).contains("has no notes"));
        assert!(refuse(one_clip("2", "C3 0 1")).contains("line 1"));
        assert!(refuse(one_clip("0", "C3 0 1 90")).contains("start"));
        assert!(refuse(one_clip("2", "C3 5 1 90")).contains("outside the clip"));
        let long = json!({ "clips": [{ "track": 1, "start": "1", "length": 33 * 4, "notes": "C3 0 1 90" }] });
        assert!(refuse(long).contains("at most 32 bars"));
        let many: Vec<Value> = (0..=MAX_CLIPS)
            .map(|i| json!({ "track": 1, "start": (i * 2 + 1).to_string(), "length": 4, "notes": "C3 0 1 90" }))
            .collect();
        assert!(refuse(json!({ "clips": many })).contains("at most 16"));
        let rows = vec!["C3 0 0.1 90"; MAX_NOTES + 1].join("\n");
        assert!(refuse(one_clip("2", &rows)).contains("notes in one call"));
        let overlapping = json!({ "clips": [
            { "track": 1, "start": "1", "length": 8, "notes": "C3 0 1 90" },
            { "track": 1, "start": "2", "length": 4, "notes": "C3 0 1 90" },
        ] });
        assert!(refuse(overlapping).contains("Clips 1 and 2 overlap"));
    }

    #[test]
    fn create_clips_reports_the_loop_and_extends_it_when_asked() {
        let mut sequencer = sequencer();
        sequencer.set_global_region(0, BAR * 2);
        let write = create(&sequencer, &one_clip("1", "C3 0 1 90")).unwrap();
        assert_says(&write.reply, "inside the loop region 1 → 3");
        let write = create(&sequencer, &one_clip("3", "C3 0 1 90")).unwrap();
        assert_says(&write.reply, "outside the loop region");

        let mut args = one_clip("3", "C3 0 1 90");
        args["extend_loop"] = json!(true);
        let write = create(&sequencer, &args).unwrap();
        assert_says(&write.reply, "now runs 1 → 4");
        let mut record = Record::new();
        record.edit(&mut sequencer, write.edit);
        assert_eq!(sequencer.region_end(), BAR * 3);
        record.undo(&mut sequencer);
        assert_eq!(sequencer.region_end(), BAR * 2);
    }

    #[test]
    fn replace_clip_notes_swaps_every_note_and_keeps_the_wheels_unless_told() {
        let mut sequencer = sequencer();
        // A carved clip: window 960..BAR + 960, a hidden note before it.
        let mut clip = clip_at(BAR, BAR);
        clip.region_mut().set_region(Some(960), Some(BAR + 960));
        for (tick, message) in [
            (0, vec![0x90, 50, 90]),
            (480, vec![0x80, 50, 0]),
            (960, vec![0x90, 60, 90]),
            (1200, vec![0xE0, 0, 0x50]),
            (1920, vec![0x80, 60, 0]),
        ] {
            clip.add_event(Event::new(tick, 0, message));
        }
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        sequencer.tracks_mut()[0].add_clip(&clip);
        let id = short_id(&clip);

        let args = json!({ "clip": id, "notes": "E4 0 1 80\nG4 3.5 2 80", "humanize": false });
        let write = replace_clip_notes(&sequencer, &args, &mut Jitter::from_seed(1)).unwrap();
        assert_says(&write.reply, "with 2 notes, wheels kept");
        assert_says(&write.reply, "1 note cut at the clip end");
        assert_eq!(write.footer, "Agent: new notes on track 1, bar 2");
        let mut record = Record::new();
        record.edit(&mut sequencer, write.edit);
        let replaced = &sequencer.tracks()[0].clips()[0];
        assert_eq!(
            notes_of(replaced),
            vec![(64, 960, 960), (67, 960 + 3360, 480)]
        );
        assert_eq!(
            replaced
                .events()
                .iter()
                .filter(|e| e.tick() == 1200)
                .count(),
            1
        );
        assert_eq!(replaced.start_tick(), BAR);
        assert_eq!(replaced.region_length(), BAR);

        record.undo(&mut sequencer);
        assert_eq!(
            notes_of(&sequencer.tracks()[0].clips()[0]),
            vec![(50, 0, 480), (60, 960, 960)]
        );

        let mut args = args;
        args["clear_wheels"] = json!(true);
        let write = replace_clip_notes(&sequencer, &args, &mut Jitter::from_seed(1)).unwrap();
        record.edit(&mut sequencer, write.edit);
        let cleared = &sequencer.tracks()[0].clips()[0];
        assert!(
            cleared.events().iter().all(|e| e.tick() != 1200),
            "the bend went"
        );
    }

    #[test]
    fn replace_clip_notes_names_an_unknown_clip() {
        let sequencer = sequencer();
        let error = replace_clip_notes(
            &sequencer,
            &json!({ "clip": "deadbeef", "notes": "C3 0 1 90" }),
            &mut Jitter::from_seed(1),
        )
        .err()
        .unwrap();
        assert_eq!(error, "Unknown clip deadbeef; call get_context again.");
    }

    #[test]
    fn set_tempo_sets_clamps_and_knows_when_nothing_changes() {
        let mut sequencer = sequencer();
        let Ok(AgentAnswer::Write(write)) = set_tempo(&sequencer, &json!({ "bpm": 92.5 })) else {
            panic!("expected a write");
        };
        assert_eq!(write.footer, "Agent: tempo 92.5 BPM");
        let mut record = Record::new();
        record.edit(&mut sequencer, write.edit);
        assert_eq!(format_bpm(sequencer.tempo_us()), "92.5");

        let Ok(AgentAnswer::Reply(reply)) = set_tempo(&sequencer, &json!({ "bpm": "92.5" })) else {
            panic!("expected a reply");
        };
        assert_says(&reply.text, "already 92.5");

        let Ok(AgentAnswer::Write(write)) = set_tempo(&sequencer, &json!({ "bpm": 900 })) else {
            panic!("expected a write");
        };
        assert_says(&write.reply, "300.0 BPM");
        assert_says(&write.reply, "clamped");
        assert!(set_tempo(&sequencer, &json!({ "bpm": -3 })).is_err());
    }
}
