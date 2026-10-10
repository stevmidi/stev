# Roadmap

What Stev may do next, and what it won't. It picks up where the release plan (`archive/240-release-plan.md`) left off; that plan ended with the first public release, `v0.1.0`, on 2026-10-09. Nothing here is scheduled. Each candidate is weighed against § Principle when it comes up. A candidate that gets picked up gets its design here, or in its own `NNN-*.md` once it outgrows a section. When it lands, its entry is removed: the behaviour as built goes into the topic docs and `CHANGELOG.md` records it.

## Principle

**Build the app the author wants to use.** Stev is an opinionated sequencer built around capture: play freely, and keep what you just played. That workflow is why the app exists. It grows in that direction, not toward a general-purpose DAW, and a feature earns its place by serving that workflow, not by whether every user would want it. Others are welcome to fork it and take it elsewhere. The licence, the agent-oriented docs and the clean platform boundary (§ Platforms) make that practical, but the repo doesn't shape itself around forks. Every addition still follows the ground rules in `AGENTS.md`: tests in the same change, docs in sync, the build gate, permissive licences only.

## Platforms

**macOS is the supported platform; Linux and Windows build in CI but are untested** (decided 2026-10-07). The plugin host, the app's strongest feature, is macOS-only, so off macOS Stev is a MIDI Out sequencer. Supporting Linux and Windows properly would mean timing work per OS, smoke tests on machines the author doesn't use, and support requests the author can't reproduce. That kind of support would quietly be neglected, which is worse than none promised. What stays is the cheap part that keeps a port possible:

- **CI compiles and tests all three** (`000` § CI).
- **The `cfg` boundary stays clean**, so a port stays a port and not an untangling job. Platform code lives behind `cfg(target_os = …)`, with no new macOS-only assumptions in the format-agnostic host core (`InstrumentMixer`, the merged catalog, instrument persistence; `130`) or anywhere else outside the platform modules. CI enforces the compiling half of this; the rest is review.

What a port would need is in `260-porting.md`. Binaries, when they come, are macOS only (§ Candidates, a downloadable macOS app).

## Candidates

### A downloadable macOS app

There is no binary today. A release carries only GitHub's source archives, and running Stev means installing Rust and the Xcode Command Line Tools, then building with `cargo run --release` (README § Building). That's fine for developers but a wall for most musicians. There are three levels, each building on the last:

1. **An unsigned `.app` in a zip.** An `Info.plist` (bundle id `com.stevmidi.stev`), an icon, and `THIRD-PARTY-NOTICES.md` inside the bundle. A GitHub Actions job builds it when a `v*` tag is pushed and attaches it to that release. Free. Gatekeeper blocks the first launch ("can't be verified"), so the README explains the one-time System Settings → Privacy & Security → *Open Anyway*.
2. **A signed and notarized `.app` / `.dmg`.** Opens without a warning. Needs the Apple Developer Program ($99/year) and signing + notarization secrets in the release workflow. Notarization requires the hardened runtime, which refuses to load code signed by anyone else. The app therefore needs the `com.apple.security.cs.disable-library-validation` entitlement, or no CLAP/VST3 plugin will load (every DAW ships with it).
3. **A Homebrew cask** (`brew install --cask stev`), on top of level 2.

Comes with it:

- **Architectures:** a universal binary (`x86_64` + `aarch64`, joined with `lipo`), so one download serves every Mac. Only Intel can be tested today, since the author's one machine is an Intel Mac. Either ship Intel-only until an Apple Silicon Mac is available, or ship universal and say the Apple Silicon build is untested.
- **Test the bundle by hand once.** The macOS-only paths (CoreAudio, the mach timer, the virtual MIDI port, Finder file drag, quit) have only ever run as a bare `cargo run` binary, never from inside a `.app`.
- **An About box** (or a settings tab) with the VST Compatible logo and the trademark sentence. Steinberg's guidelines say the logo should appear there (`080` § Licensing).
- **stevmidi.com** pointed at GitHub Pages or the repo, carrying the logo on its pages; then set it as the repo's website.
- **A log file** in `~/Library/Logs/stev/` behind the `log` facade, replacing `dprintln!` where a user's bug report needs it. Deferred until now on purpose: it matters once strangers run a build they didn't compile.

Suggested start: level 1, when there's a release worth handing to non-developers.

### Export to audio

Render the arrangement, or the loop region, to a WAV file, so a sketch can be shared without a DAW (added 2026-10-10). Open questions for its design:

- **Only plugin tracks can be rendered.** A MIDI Out track's audio is made by external gear or another app and never reaches Stev, so the export says which tracks it left out, or refuses when nothing would be heard. Like the plugin host, it's macOS-only.
- **Offline or real time.** Offline (driving the voices faster than real time, off the audio thread) is quick and glitch-free, but each plugin must accept the CLAP/VST3 offline-processing flag, and the render must reproduce the transport exactly as playback does (`BlockTransport`, note chase, the wheels). Real time (recording the output bus while playing) is simpler and exactly what you hear, but takes as long as the song and records any overrun.
- **The tail:** how long to keep rendering past the end for reverb and release tails (a fixed tail, or until silence).
- **Format:** WAV at the device's sample rate, 24-bit or 32-bit float. A writer is small enough to hand-roll, as the SMF writer was (`smf.rs`), with no dependency.
- **Scope:** the mix only, or a stem per track. Stems fit the "one clip / one track at a time" style of the MIDI export (`060` § MIDI clip export).
- **Where it lands:** in the project folder without a dialog, never overwriting, as the MIDI clip export does.

### Detecting the meter on Enter

Idea, 2026-10-09; not prioritized. Enter's fit (`fit_first_clip_tempo`, `040` § Fitting the tempo) could guess the first clip's meter together with its tempo. Feasible in part; worth a cheap probe before any design.

- **Why the length alone can't do it:** the fit uses only the played length, and any length fits any meter once the tempo is free. For example, 3 s is 2 bars of 4/4 at 160, or of 3/4 at 120, or of 6/8 at 120. Today Enter assumes the project's meter and lets the octave band pick the tempo. The meter has to come from what's played *inside* the phrase.
- **The cues:**
  1. **The beat.** Onset intervals cluster around a pulse. Stev has no beat finder today: the tempo fit divides the length by a whole number of bars and never looks inside. This is the missing piece.
  2. **Beats per phrase: the strongest cue, and a natural fit for capture.** Players close phrases on 1, 2, 4 or 8 bars, so once the beat is known, 8 or 16 beats → 4/4; 6, 12 or 24 → 3/4 (or 6/8); 7 or 14 → 7/x; 5 or 10 → 5/x. Twelve could be three bars of 4/4, but a "phrases are 1/2/4/8 bars" prior settles most of those.
  3. **Accents.** Strong beats tend to get louder, longer and lower notes, chord changes and bass notes. A pattern repeating every 2 beats reads duple, every 3 triple.
  4. **3/4 vs 6/8.** Same bar length at the same quarter tempo; only the grouping differs. Onsets and accents fall at 0 / 960 / 1920 ticks for 3/4, and at 0 / 1440 with eighths in threes for 6/8. A weaker signal, and partly taste.
- **Not detectable, never offered:** 4/4 vs 2/4 vs 2/2 (the tempo-octave ambiguity again, and musically irrelevant); borderline 3/4 vs 6/8 and heavily syncopated playing, where two musicians would disagree. A realistic expectation: duple vs triple fairly reliable, odd meters when the phrase repeats cleanly, 3/4 vs 6/8 a coin flip.
- **Shape if built:** a small hypothesis set (4/4, 3/4, 6/8, maybe 5/4 and 7/8), each scored as a (meter, tempo) pair on whether its beat count makes a plausible whole-bar phrase. **Keep the project's meter unless the evidence is strong.** A wrong 6/8 annoys far more than a kept 4/4, and the meter chip fixes it cheaply. Undo restores meter and tempo together, as the fit's undo does today. Only the first clip, as with the fit.
- **Pinned to real takes**, as phrase detection is (`040` § Phrase start). Fixtures already record `meter`, so the meter the user chose for a take is the answer the detector must reproduce. Every checked-in 4/4 take must come out 4/4 before a 3/4 result counts.
- **First step:** a test-only probe over `fixtures/captures/` that finds the beat, counts the beats in the phrase and reports duple/triple, changing no app behaviour. If it can't call every 4/4 take duple, the idea dies cheaply. If it can, record a few 3/4 and 6/8 takes, pin them, and see whether triple comes out.

### Adjusting the detected phrase after a commit

A stopped `/` commits detection's best guess, and a take framed too long or too short can't be corrected the way the old pending view allowed. To be solved in the clip view, in its own design doc before any code. The gap and the questions for that doc are in `220` § Known gap.

### Keyboard shortcuts as a settings tab, with rebinding

User direction, 2026-10-07. The `?` help overlay is the first-release form. Later it moves into the settings modal as its own tab: a categorized, searchable list of every binding that can also record a new binding for each one. Rebinding likely means the matchers read from a keymap instead of matching keys inline, which would also retire the hand-written `HELP` table (`help_overlay.rs`).

### Track activity, then level meters

Audio meters only mean something on plugin tracks: a MIDI Out track's audio never reaches the app, so they'd be blank on most tracks and missing off macOS. A per-track MIDI-activity indicator works on every track and platform, and is the more general first step. A full mixer panel stays § Not planned.

### Smaller loose ends

- **The audio workgroup join.** Worker threads have no real-time priority, and under CPU pressure their pre-emption is the dominant cause of glitches (the overrun journal shows it). The fix and its acceptance test are in `170` § Not done yet.
- **Quit from the Dock or a logout** skips the unsaved-changes prompt; only ⌘Q and closing the window are covered (`060` § Unsaved changes).
- **Replacing a track's plugin isn't undoable.** The old plugin's preset state is gone, and the footer says so (`130`).
- **No hint that a clip carries wheel data.** Pitch bend and mod wheel play back but are invisible in the piano roll and the thumbnails (`090`).

### MIDI Out timing

Parked 2026-10-07 with the macOS-first decision. The analysis, the timing-probe design and the fixes are in `260-porting.md` § Timing, since they are a port's first job. If picked up here, it starts on macOS alone: the probe measures macOS as well, the precise `"midiout"` wait helps every platform (crossbeam's `select!` timeout is a condvar wait on macOS too), and macOS threads have no real-time priority either. The author's own test plan:

- **The machines:** the Windows 11 machine, booted both natively and from an **Ubuntu 24.04 live USB stick**, so Windows and Linux are compared on the same hardware; the Intel Mac as the reference (the mach timer path); a **Surface Pro 3 on Windows 10** as the old-laptop worst case (2014 low-power Intel that throttles when hot: run it plugged in, on the *High performance* power plan, cooled between loaded runs; a first release build may need `-j 2` on 4 GB of RAM); and, optionally, the headless Raspberry Pi 4. Windows 11 stays the main Windows machine. Unlike 10, it stops honouring a process's timer-resolution request while that process's window is minimised or hidden, so the `timeBeginPeriod` fix (`260`) must be checked on 11, in the app, with its window minimised. On the live stick nothing persists across a reboot: `apt install build-essential pkg-config libasound2-dev git`, install rustup (the pinned toolchain installs itself), and clone the repo.
- **Loopback hardware:** a Roland UM-ONE with its Out plugged into its own In, on every machine (the Scarlett 2i2 has no MIDI).
- **The baseline**, filled in from the runs:

  | Machine | Load | Clock mean / p99 / max | `midiout` recorder p99 / max | Loopback p99 / max |
  |---|---|---|---|---|
  | Intel Mac (reference) | idle / loaded | | | |
  | Windows 11 | idle / loaded | | | |
  | Ubuntu 24.04 (same machine) | idle / loaded | | | |
  | Surface Pro 3, Windows 10 | idle / loaded | | | |
  | Raspberry Pi 4 (optional) | idle / loaded | | | — |

## Not planned

Mirrored in the README's *Not planned* list, so nobody expects these. Some may come later if the capture workflow needs them, but none is on the roadmap.

- tempo or meter changes along the timeline (one tempo and one meter per project; the meter is stored as a list so changes could extend the format later, `060`)
- audio tracks and audio recording
- automation lanes and CC lane editing
- effect plugins, sends, a mixer panel
- MIDI clock / Ableton Link / external sync
- notation
- whole-arrangement MIDI file export and multi-track import (one clip at a time each way, `060`)
- a richer browser: search, tags, favourites, preview playback, sample/preset categories (the browser has a Projects category of folders, `.stev` and `.mid` files, and a flat Plugins category)
- richer piano-roll editing: a draw/pencil mode, paint-drag, erase gestures, ⌥-drag duplicate, unsnapped drawing, drawing velocity in the velocity panel, Shift-click to add to the selection, drawing clips in the arranger
- more than 16 tracks, track reordering, track groups/folders
- DAW integration: Mackie Control or other control-surface sync (removed 2026-10-07; `archive/120-daw-track-sync.md`)
- **official Linux and Windows support** (§ Platforms). They build and pass the tests in CI; nothing more is promised. What a port would need is in `260-porting.md`.

**Never in scope:** VST2 or any VST format older than VST3. The VST2 SDK is no longer licensed by Steinberg and cannot be redistributed under `MIT OR Apache-2.0`; only VST3 (MIT since SDK 3.8) and newer formats. See `180-vst3-host.md`.
