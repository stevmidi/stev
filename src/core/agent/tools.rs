//! What the server tells a client about itself: the tools it lists
//! (`tools/list`) and the instructions sent with `initialize`. The texts are
//! written for the model reading them (`360-mcp-server.md` § Tools and
//! § Server instructions).

use serde_json::{Value, json};

use super::reads::{NOTES_MAX_BARS, TIMELINE_MAX_BARS};

/// The instructions the `initialize` result carries: clients put them in
/// the model's context while the server is connected, so they stay short.
/// The design's text, less what only the writing tools need, until they
/// exist (`360` § Build).
pub(crate) const INSTRUCTIONS: &str = "\
Stev is a MIDI sequencer built around capture: the user plays freely and keeps
what they played. You help them build on it. You can't hear anything, so read
the music before saying anything about it.

Units. Lengths are in beats (quarter notes, whatever the meter), as decimals.
Positions are bar+beats: \"17+2\" is bar 17, two beats in; bars count from 1.
Notes are rows \"pitch start length velocity\", one per line, start in beats
from the start of the clip or range, pitch as a name with C4 = MIDI 60 (sharps
only), e.g. \"A2 0 1.5 96\". Sharps are only how Stev writes pitches, not
a spelling: in a flat key read A# as Bb and D# as Eb, and name notes and
chords as the key spells them.

How to work:
1. Call get_context first on every request. \"This\" means the marquee it
   reports; without one, the loop region. Ask if neither fits the request.
2. Read before answering: get_timeline for the harmony from what sounds,
   get_notes for the parts themselves, get_project for anything outside the
   marquee.
3. This version of Stev can only read. If asked to write notes, give them as
   rows and say that writing into the project isn't available yet.

Stev doesn't make tracks, pick instruments or mix. If asked, say so: the user
does that in the app.";

/// The names of every tool the server offers.
pub(crate) const TOOL_NAMES: [&str; 4] =
    ["get_context", "get_project", "get_timeline", "get_notes"];

/// The `from` / `to` / `tracks` properties the range reads share.
fn range_properties(max_bars: i32, tracks_default: &str) -> Value {
    json!({
        "from": {
            "type": ["string", "integer"],
            "description": format!(
                "Start position, e.g. \"9\" or \"9+2.5\". Default: the marquee's \
                 start, else the loop region's. At most {max_bars} bars from from to to."
            ),
        },
        "to": {
            "type": ["string", "integer"],
            "description": "End position, exclusive, e.g. \"17\". Pass with from.",
        },
        "tracks": {
            "type": "array",
            "items": { "type": ["string", "integer"] },
            "description": format!("Track numbers (from 1) or names. Default: {tracks_default}."),
        },
    })
}

/// The `tools/list` result's `tools` array.
pub(crate) fn tool_list() -> Value {
    let mut notes_properties = range_properties(
        NOTES_MAX_BARS,
        "the marqueed tracks, else every track that isn't muted",
    );
    notes_properties["include_wheels"] = json!({
        "type": "boolean",
        "description": "Also list pitch bend and mod wheel moves as \
                        \"bend <beat> <value>\" (centre 0) and \"mod <beat> <value>\" rows.",
    });
    json!([
        {
            "name": "get_context",
            "description": "What the user is pointing at. Call it first, on every request: \
                the user may have moved the marquee since your last call. Gives the tempo, \
                meter, bar length in beats, playing or stopped, the cursor, the loop region, \
                then the marquee (the selection the user means by \"this\") with, per \
                marqueed track, whether it has notes there, the clips it touches (id, range, \
                swing) and the next clip after it, then the selected track and lead clip.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "get_project",
            "description": "The whole project at a glance, without notes: for requests \
                about something outside the marquee. Each track (number, name, output, \
                muted, soloed) and each clip (id, track, range, note count, muted, swing, \
                pitch range as a hint at its role).",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "get_timeline",
            "description": "What actually sounds, bar by bar, with clip windows, mutes and \
                solo applied: per beat the pitches each track sounds (a beat is listed when \
                a note starts or what sounds changes; \"-\" is silence), and per bar the \
                pitch classes of all tracks together with the lowest note. Use it to read \
                the harmony. Muted tracks are named once, not listed.",
            "inputSchema": {
                "type": "object",
                "properties": range_properties(TIMELINE_MAX_BARS, "every track that isn't muted"),
            },
        },
        {
            "name": "get_notes",
            "description": "The notes tracks play over a range, as rows \
                \"pitch start length velocity\" with starts in beats from from, whichever \
                clips they come from. Clip windows and muted notes are applied, so the rows \
                are what is heard; a note already sounding at from has a negative start. \
                Use it to read a part's rhythm, lengths and velocities.",
            "inputSchema": { "type": "object", "properties": notes_properties },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_listed_is_named_and_has_an_object_schema() {
        let tools = tool_list();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, TOOL_NAMES);
        for tool in tools.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
    }
}
