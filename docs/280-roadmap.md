# Roadmap

What Stev may do next, and what it won't. It picks up where the release plan (`archive/240-release-plan.md`) left off; that plan ended with the first public release, `v0.1.0`, on 2026-10-09. Nothing here is scheduled, but § Candidates is in priority order. Each candidate is weighed against § Principle when it comes up. A candidate that gets picked up gets its design here, or in its own `NNN-*.md` once it outgrows a section. When it lands, its entry is removed: the behaviour as built goes into the topic docs and `CHANGELOG.md` records it.

## Principle

**Build the app the author wants to use.** Stev is an opinionated sequencer built around capture: play freely, and keep what you just played. That workflow is why the app exists. It grows in that direction, not toward a general-purpose DAW, and a feature earns its place by serving that workflow, not by whether every user would want it. **Stev arranges; a DAW mixes.** There is no mixing DSP in the app (no EQ, compression or other effects), and that's a strength: the user stays on the arrangement. Per-track volume, pan, mute and solo stay, for hearing a sketch balanced while playing, and the hand-off to a DAW is a tidy stem set (`130` § Audio export). Others are welcome to fork it and take it elsewhere. The licence, the agent-oriented docs and the clean platform boundary (§ Platforms) make that practical, but the repo doesn't shape itself around forks. Every addition still follows the ground rules in `AGENTS.md`: tests in the same change, docs in sync, the build gate, permissive licences only.

## Platforms

**macOS is the supported platform; Linux and Windows build in CI but are untested** (decided 2026-10-07). The plugin host, the app's strongest feature, is macOS-only, so off macOS Stev is a MIDI Out sequencer. Supporting Linux and Windows properly would mean timing work per OS, smoke tests on machines the author doesn't use, and support requests the author can't reproduce. That kind of support would quietly be neglected, which is worse than none promised. What stays is the cheap part that keeps a port possible:

- **CI compiles and tests all three** (`000` § CI).
- **The `cfg` boundary stays clean**, so a port stays a port and not an untangling job. Platform code lives behind `cfg(target_os = …)`, with no new macOS-only assumptions in the format-agnostic host core (`InstrumentMixer`, the merged catalog, instrument persistence; `130`) or anywhere else outside the platform modules. CI enforces the compiling half of this; the rest is review.

What a port would need is in `260-porting.md`. Binaries are macOS only (`archive/330-macos-app.md`).

Full Linux and Windows support is now a staged candidate (§ Candidates, Linux and Windows support, staged; added 2026-10-11). Until a stage lands, the above holds: nothing beyond CI is promised.

## Candidates

Listed in priority order, highest first (set 2026-10-11): the capture workflow, then the engine's real-time threads, small fixes, then reaching non-developers, then larger or speculative features. The order is a priority, not a schedule; reorder it as priorities change.

### MIDI Out timing

Stage 1 of § Linux and Windows support, staged, taken first as macOS work. Parked 2026-10-07 with the macOS-first decision, picked up 2026-10-14: the probe and all three fixes (the precise `"midiout"` wait, the absolute-deadline clock timer, real-time priority for both timing threads) are built and measured on macOS (`260` § The fixes); the Linux and Windows rows below are what's left. The analysis, the timing-probe design and the fixes are in `260-porting.md` § Timing, since they are a port's first job. The author's own test plan for the rest:

- **The machines:** the Windows 11 machine, booted both natively and from an **Ubuntu 24.04 live USB stick**, so Windows and Linux are compared on the same hardware; the Intel Mac as the reference; a **Surface Pro 3 on Windows 10** as the old-laptop worst case (2014 low-power Intel that throttles when hot: run it plugged in, on the *High performance* power plan, cooled between loaded runs; a first release build may need `-j 2` on 4 GB of RAM); and, optionally, the headless Raspberry Pi 4. Windows 11 stays the main Windows machine. Unlike 10, it stops honouring a process's timer-resolution request while that process's window is minimised or hidden, so the `timeBeginPeriod` fix (`260`), if the probe shows it's needed, must be checked on 11, in the app, with its window minimised. On the live stick nothing persists across a reboot: `apt install build-essential pkg-config libasound2-dev git`, install rustup (the pinned toolchain installs itself), and clone the repo.
- **Loopback hardware:** a Roland UM-ONE with its Out plugged into its own In, on every machine (the Scarlett 2i2 has no MIDI).
- **The baseline**, filled in from the runs:

  | Machine | Load | Clock mean / p99 / max | `midiout` recorder p99 / max | Loopback p99 / max |
  |---|---|---|---|---|
  | Intel Mac (reference) | idle / loaded | measured, `260` § The fixes | measured, `260` § The fixes | virtual port only, `260` § The fixes |
  | Windows 11 | idle / loaded | | | |
  | Ubuntu 24.04 (same machine) | idle / loaded | | | |
  | Surface Pro 3, Windows 10 | idle / loaded | | | |
  | Raspberry Pi 4 (optional) | idle / loaded | | | — |

### A signed macOS app

Level 1, an unsigned universal `Stev.app` in a zip attached to each release by CI, is built (`archive/330-macos-app.md`, 2026-10-25). Its first launch needs Gatekeeper's *Open Anyway* (README § Download), once per version: the approval belongs to one ad-hoc-signed build, so every update asks again (seen 0.6.0 → 0.7.0, 2026-10-26). The README gives `xattr -dr com.apple.quarantine` as the Terminal shortcut. The Apple Silicon slice is untested: the author's one machine is an Intel Mac, so an Apple Silicon bug report is the first signal to act on. The levels left build on it:

2. **A signed and notarized `.app` / `.dmg`.** Opens without a warning. Needs the Apple Developer Program ($99/year) and signing + notarization secrets in the release workflow. Notarization requires the hardened runtime, which refuses to load code signed by anyone else. The app therefore needs the `com.apple.security.cs.disable-library-validation` entitlement, or no CLAP/VST3 plugin will load (every DAW ships with it).

   Decided 2026-10-25: not yet. The fee recurs for as long as releases are signed, and nothing yet shows who downloads Stev. Level 1's *Open Anyway* is asked once per version, so each update costs users the same steps, and the bundle script and release workflow carry over, with signing slotted in at the end. Enrol when one of these happens:
   - People report trouble with *Open Anyway*, at the first launch or after an update, or someone is heard to have given up at it.
   - The Homebrew cask (level 3) is wanted. Homebrew has been phasing out casks that fail Gatekeeper, so check its current policy then.
   - Stev is put in front of a wider audience: a forum post, a video, a store-like listing.

   An individual account signs as the author's own name. Signing as "Stev" needs a legal entity and a D-U-N-S number.
3. **A Homebrew cask** (`brew install --cask stev`), on top of level 2.

A **self-updater** (Sparkle) downloading and installing in place needs level 2's signing plus an EdDSA-signed appcast, and overlaps with level 3's cask: optional after level 2, not before. The manual Check for updates (`archive/330` § 3) stays until then.

### Export to audio: what's left

The stem export is built (`⌘/Ctrl+⇧+R`, as built in `130` § Audio export, phase plan in `archive/300-export-to-audio.md`). Two follow-ups were left out of it on purpose:

- **Offline rendering:** quick and glitch-free, but each plugin must accept the CLAP/VST3 offline-processing flag, and the render must reproduce the transport exactly as playback does (`BlockTransport`, note chase, the wheels).
- **Tails that end on silence:** the files ringing out past the selection until the output is measured silent, and the export waiting for the same silence before it starts. Designed as phase 4 of `archive/300-export-to-audio.md` (detector, thresholds, caps) and deferred 2026-10-19: DAW users don't expect it, Ableton doesn't do it, and the selection already sets the length. If a tail from playback just before the start ever reaches the files, try its smaller cousin first: a short fixed pause before the start.

### Linux and Windows support, staged

Added 2026-10-11: everything macOS has today, on Linux and Windows too, built in stages that each stand on their own. It is by far the largest candidate: a second and third platform, not a feature. The ongoing cost matters more than the code. The author doesn't use either OS day to day, so timing and plugin bugs there are found on machines booted only to test, which is the neglect § Platforms warns about. So each stage is a promise made only once it's done, and stopping after any stage leaves an honest state. What's macOS-only today, and why each part matters, is in `260-porting.md`.

1. **Timing: the probe and the fixes** (§ MIDI Out timing above). The timing probe, the precise `"midiout"` deadline wait, one absolute-deadline clock timer, and best-effort thread priority. All of it helped macOS too, so it was done as macOS work ahead of the rest of this candidate (2026-10-14); what's left is measuring Linux and Windows. It clears the first and largest porting hurdle.
2. **MIDI Out parity on Windows 11 and Ubuntu 24.04.** Run the smoke test (`260` § Smoke test) and fix what it finds: no virtual MIDI port on Windows (midir can't create one; at least document loopMIDI or Windows MIDI Services), quit with unsaved changes, `.mid` file drag, MIDI hot-plug, and the library and log folders. When this lands, Stev is a supported MIDI Out sequencer on all three, with plugins on macOS only, and the README, § Platforms and § Not planned say so.
3. **The plugin host, Windows first.** Windows has far more musicians and VST3 plugins, and the author has a Windows 11 machine. The host core is format-agnostic (`130`); what's needed is the platform glue: plugin search paths per OS, VST3 bundle layout (`180`), editor windows parented to Win32 (and X11 later), MMCSS as the workgroup join's counterpart (`170`), and expect another round of the GL-context clash with plugins that bring their own GL (NI, Qt). Linux CLAP/VST3 follows Windows; its plugin ecosystem is thin. Decide on this stage only after living with stage 2.
4. **Packaging per OS** (installer or archive per platform, as `archive/330-macos-app.md` does for macOS), only once stage 3 is solid.

Stage 1 is § MIDI Out timing, listed with the engine work above. Stages 2–4 come after the downloadable macOS app (built 2026-10-25, `archive/330`).

### Website extras

The site is live at stevmidi.com (2026-10-22), with the four capture loops in its Capture section since 2026-10-24; both as built in `000` § Website. Two options remain, neither promised: a loop beside or in place of the hero screenshot, and versions of the loops with sound, which need a loopback driver to record the app's output.

**GitHub Discussions.** The site's *Take part* section sends bugs to the issue form and feature ideas to an issue after `CONTRIBUTING.md` § What fits. Discussions, with an Ideas and a Q&A category, would keep ideas and questions out of the issue list, which would then hold only bugs and agreed work, and an idea is easier to decline there. Decided 2026-10-28: not yet. An empty Discussions tab makes a new one-person project look quieter than it is, and a few users split across two places make both look emptier. Turning it on takes a minute, and GitHub converts an issue to a discussion, so nothing filed before then is lost. Turn it on when either of these happens:

- feature ideas or "how do I…" questions start crowding the bug reports in Issues;
- ideas get closed as not planned often enough that a softer place to decline them would help.

It then also changes the *Suggest a feature* card on the site, the feature line in `.github/ISSUE_TEMPLATE/config.yml`, `CONTRIBUTING.md` § What fits and `000` § Website. Private notes stay in the author's Obsidian vault either way.

**Donations.** A *Support with a donation* card beside the three in *Take part*, as IINA has, backed by GitHub Sponsors and a `.github/FUNDING.yml` so the repo shows a Sponsor button too. Decided 2026-10-28: not yet. With no traction a donation link earns nothing and reads as asking before giving. Revisit when the project has traction: reports or pull requests from people other than the author arriving regularly, or release downloads showing steady users. The first thing donations would pay for is the Apple Developer fee behind a signed app (§ A signed macOS app). Setting it up then means enrolling in Sponsors, which handles payouts and tax forms, adding `FUNDING.yml`, the card and its line in `000` § Website.

## Not planned

Mirrored in the README's *Not planned* list, so nobody expects these. Some may come later if the capture workflow needs them, but none is on the roadmap.

- tempo or meter changes along the timeline (one tempo and one meter per project; the meter is stored as a list so changes could extend the format later, `060`)
- detecting the meter from a take: Enter fits the tempo in the project's meter and the meter chip sets it. Probed 2026-10-27 over the pinned takes and dropped. A free noodle doesn't reveal its meter: its phrase ends on a rest of unknown length, so 10–12 played eighths fit two bars of 3/4 as well as two of 4/4, and the accents were at noise level. A cleanly repeating riff does (one 3/4 test take repeated its rhythm exactly every three beats), but its whole-bar length fits 4/4 just as well, and a three-beat riff can be meant over 4/4. Too narrow and too subjective to save one click on the chip; Ableton's Capture doesn't detect the meter either
- audio tracks and audio recording
- a built-in sampler or other built-in instrument: sampler and instrument plugins cover it, loaded through the plugin host (`130`)
- track automation lanes and drawing controller data: the piano roll shows the clip's pitch bend, mod wheel and knob CCs in a lane (`380`, `390`) that only clears a span or the whole controller; no lane draws values and no automation lives on the arranger's tracks
- mixing: built-in EQ, compression or other effects, effect plugins, sends, a mixer panel (Stev arranges, a DAW mixes: § Principle; the hand-off is a stem export)
- MIDI clock / Ableton Link / external sync
- notation
- whole-arrangement MIDI file export and multi-track import (one clip at a time each way, `060`)
- a richer browser: search, tags, favourites, preview playback, sample/preset categories (the browser has a Projects category of folders, `.stev` and `.mid` files, and a flat Plugins category)
- richer piano-roll editing: a draw/pencil mode, paint-drag, erase gestures, ⌥-drag duplicate, unsnapped drawing, drawing velocity in the velocity panel, Shift-click to add to the selection, drawing clips in the arranger
- more than 16 tracks, track reordering, track groups/folders
- DAW integration: Mackie Control or other control-surface sync (removed 2026-10-07; `archive/120-daw-track-sync.md`)

**Never in scope:** VST2 or any VST format older than VST3. The VST2 SDK is no longer licensed by Steinberg and cannot be redistributed under `MIT OR Apache-2.0`; only VST3 (MIT since SDK 3.8) and newer formats. See `180-vst3-host.md`.
