# Roadmap

What Stev may do next, and what it won't. It picks up where the release plan (`archive/240-release-plan.md`) left off; that plan ended with the first public release, `v0.1.0`, on 2026-10-09. Nothing here is scheduled, but § Candidates is in priority order. Each candidate is weighed against § Principle when it comes up. A candidate that gets picked up gets its design here, or in its own `NNN-*.md` once it outgrows a section. When it lands, its entry is removed: the behaviour as built goes into the topic docs and `CHANGELOG.md` records it.

## Principle

**Build the app the author wants to use.** Stev is an opinionated sequencer built around capture: play freely, and keep what you just played. That workflow is why the app exists. It grows in that direction, not toward a general-purpose DAW, and a feature earns its place by serving that workflow, not by whether every user would want it. **Stev arranges; a DAW mixes.** There is no mixing DSP in the app (no EQ, compression or other effects), and that's a strength: the user stays on the arrangement. Per-track volume, pan, mute and solo stay, for hearing a sketch balanced while playing, and the hand-off to a DAW is a tidy stem set (`130` § Audio export). Others are welcome to fork it and take it elsewhere. The licence, the agent-oriented docs and the clean platform boundary (§ Platforms) make that practical, but the repo doesn't shape itself around forks. Every addition still follows the ground rules in `AGENTS.md`: tests in the same change, docs in sync, the build gate, permissive licences only.

## Platforms

**macOS is the supported platform; Linux and Windows build in CI but are untested** (decided 2026-10-07). The plugin host, the app's strongest feature, is macOS-only, so off macOS Stev is a MIDI Out sequencer. Supporting Linux and Windows properly would mean timing work per OS, smoke tests on machines the author doesn't use, and support requests the author can't reproduce. That kind of support would quietly be neglected, which is worse than none promised. What stays is the cheap part that keeps a port possible:

- **CI compiles and tests all three** (`000` § CI).
- **The `cfg` boundary stays clean**, so a port stays a port and not an untangling job. Platform code lives behind `cfg(target_os = …)`, with no new macOS-only assumptions in the format-agnostic host core (`InstrumentMixer`, the merged catalog, instrument persistence; `130`) or anywhere else outside the platform modules. CI enforces the compiling half of this; the rest is review.

What a port would need is in `260-porting.md`. Binaries, when they come, are macOS only (§ Candidates, a downloadable macOS app).

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

### A website at stevmidi.com

The domain is held but points nowhere yet (added 2026-10-10). Today the GitHub README is the only front page, which suits developers but not the musicians a downloadable app (§ below) is for. Open questions for its design:

- **What it's for.** A front page that says what Stev is in one screen: the capture workflow (play freely, keep what you just played), a short screen recording or a few screenshots, a download button once there's a binary, and links to the repo, the changelog and the licence. Not a second copy of the docs: the `docs/` files are written for contributors and coding agents, and the site links to them rather than republishing them.
- **The look.** It borrows the app's own theme (`030` § Theme & Contrast: the palette, the grid, the clip colours), so the site and the app read as one thing. A logo and an icon are needed for the `.app` bundle anyway; the site is where they're designed first.
- **Hosting.** GitHub Pages from the repo (a `site/` folder or a `gh-pages` branch, deployed by an Actions job), with stevmidi.com as its custom domain over HTTPS. Free, and no server to keep. A static page with no build step (plain HTML/CSS) unless it grows enough to need one.
- **What it promises.** The same as the README: macOS supported, Linux and Windows build-only (§ Platforms), and the § Not planned list, so the site never promises more than the repo does. Any VST3 mention carries the trademark sentence and logo rules (`080` § Licensing).
- **Assets.** Screenshots and recordings are the author's own; fonts and images follow the same licence rule as the repo (`080` § Licensing), with any third-party font licence committed beside it.

**First page built** (2026-10-21, `feature/website`): `site/index.html` + `site/style.css`, plain HTML/CSS with no build step, in the Magenta Pulse palette (the theme of the README screenshot) and Plex Regular only, as the app draws (`030` § Fonts). The steps are drawn as clips (`030` § Clip Anatomy), the hero sits on a bar-and-beat grid. The screenshot, the VST logo and the font are symlinks into `docs/images/` and `assets/fonts/`, so nothing is duplicated; the Pages upload follows them. `.github/workflows/pages.yml` publishes `site/` on a push to `main` that touches it (or by hand), and `ci.yml` skips `site/**`. Until a binary exists the main button is *Build from source*; it becomes the download when level 1 of § A downloadable macOS app lands.

Left to do, outside the repo: in the repo's Settings → Pages, set the source to *GitHub Actions* and the custom domain to `stevmidi.com` with *Enforce HTTPS*; at the registrar, point the apex at GitHub Pages (`A` 185.199.108–111.153, `AAAA` 2606:50c0:8000–8003::153) and `www` at `stevmidi.github.io` (`CNAME`); verify the domain under the org's Settings → Pages. Then set the repo's website field to `https://stevmidi.com` and link it from the README. **The logo** (2026-10-22): a robin cut into four bands, the four lines of a verse; the third, across the breast, is pink: the take you kept. `site/images/logo.svg` is the master (a clip path for the bird, a mask for the eye, so any background shows through); `site/favicon.svg` follows the tab's light or dark scheme, with `favicon-32.png` and a 180 px `apple-touch-icon.png` as raster fallbacks. It combines the two favourites of a long brainstorm, four plain lines and a flat robin; the bands are 8.5 units thick so they stay apart at 16 px. The `.app` icon (§ A downloadable macOS app) starts from the same mark on the app's background, as the touch icon does. Still open: a short screen recording of a capture.

### A downloadable macOS app

There is no binary today. A release carries only GitHub's source archives, and running Stev means installing Rust and the Xcode Command Line Tools, then building with `cargo run --release` (README § Building). That's fine for developers but a wall for most musicians. There are three levels, each building on the last:

1. **An unsigned `.app` in a zip.** An `Info.plist` (bundle id `com.stevmidi.stev`), an icon, and `THIRD-PARTY-NOTICES.md` inside the bundle. A GitHub Actions job builds it when a `v*` tag is pushed and attaches it to that release. Free. Gatekeeper blocks the first launch ("can't be verified"), so the README explains the one-time System Settings → Privacy & Security → *Open Anyway*.
2. **A signed and notarized `.app` / `.dmg`.** Opens without a warning. Needs the Apple Developer Program ($99/year) and signing + notarization secrets in the release workflow. Notarization requires the hardened runtime, which refuses to load code signed by anyone else. The app therefore needs the `com.apple.security.cs.disable-library-validation` entitlement, or no CLAP/VST3 plugin will load (every DAW ships with it).
3. **A Homebrew cask** (`brew install --cask stev`), on top of level 2.

Comes with it:

- **Architectures:** a universal binary (`x86_64` + `aarch64`, joined with `lipo`), so one download serves every Mac. Only Intel can be tested today, since the author's one machine is an Intel Mac. Either ship Intel-only until an Apple Silicon Mac is available, or ship universal and say the Apple Silicon build is untested.
- **Test the bundle by hand once.** The macOS-only paths (CoreAudio, the clock timer, the virtual MIDI port, Finder file drag, quit) have only ever run as a bare `cargo run` binary, never from inside a `.app`.
- **An About box** (or a settings tab) with the VST Compatible logo and the trademark sentence. Steinberg's guidelines say the logo should appear there (`080` § Licensing).
- **stevmidi.com** carrying the logo on its pages, and set as the repo's website (§ A website at stevmidi.com).
- **A log file** in `~/Library/Logs/stev/` behind the `log` facade, replacing `dprintln!` where a user's bug report needs it. Deferred until now on purpose: it matters once strangers run a build they didn't compile.

Suggested start: level 1, when there's a release worth handing to non-developers.

### Detecting the meter on Enter

Idea, 2026-10-09. Enter's fit (`fit_first_clip_tempo`, `040` § Fitting the tempo) could guess the first clip's meter together with its tempo. Feasible in part; worth a cheap probe before any design.

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

### Export to audio: what's left

The stem export is built (`⌘/Ctrl+⇧+R`, as built in `130` § Audio export, phase plan in `archive/300-export-to-audio.md`). Two follow-ups were left out of it on purpose:

- **Offline rendering:** quick and glitch-free, but each plugin must accept the CLAP/VST3 offline-processing flag, and the render must reproduce the transport exactly as playback does (`BlockTransport`, note chase, the wheels).
- **Tails that end on silence:** the files ringing out past the selection until the output is measured silent, and the export waiting for the same silence before it starts. Designed as phase 4 of `archive/300-export-to-audio.md` (detector, thresholds, caps) and deferred 2026-10-19: DAW users don't expect it, Ableton doesn't do it, and the selection already sets the length. If a tail from playback just before the start ever reaches the files, try its smaller cousin first: a short fixed pause before the start.

### Linux and Windows support, staged

Added 2026-10-11: everything macOS has today, on Linux and Windows too, built in stages that each stand on their own. It is by far the largest candidate: a second and third platform, not a feature. The ongoing cost matters more than the code. The author doesn't use either OS day to day, so timing and plugin bugs there are found on machines booted only to test, which is the neglect § Platforms warns about. So each stage is a promise made only once it's done, and stopping after any stage leaves an honest state. What's macOS-only today, and why each part matters, is in `260-porting.md`.

1. **Timing: the probe and the fixes** (§ MIDI Out timing above). The timing probe, the precise `"midiout"` deadline wait, one absolute-deadline clock timer, and best-effort thread priority. All of it helped macOS too, so it was done as macOS work ahead of the rest of this candidate (2026-10-14); what's left is measuring Linux and Windows. It clears the first and largest porting hurdle.
2. **MIDI Out parity on Windows 11 and Ubuntu 24.04.** Run the smoke test (`260` § Smoke test) and fix what it finds: no virtual MIDI port on Windows (midir can't create one; at least document loopMIDI or Windows MIDI Services), quit with unsaved changes, `.mid` file drag, MIDI hot-plug, and the library and log folders. When this lands, Stev is a supported MIDI Out sequencer on all three, with plugins on macOS only, and the README, § Platforms and § Not planned say so.
3. **The plugin host, Windows first.** Windows has far more musicians and VST3 plugins, and the author has a Windows 11 machine. The host core is format-agnostic (`130`); what's needed is the platform glue: plugin search paths per OS, VST3 bundle layout (`180`), editor windows parented to Win32 (and X11 later), MMCSS as the workgroup join's counterpart (`170`), and expect another round of the GL-context clash with plugins that bring their own GL (NI, Qt). Linux CLAP/VST3 follows Windows; its plugin ecosystem is thin. Decide on this stage only after living with stage 2.
4. **Packaging per OS** (installer or archive per platform, as § A downloadable macOS app does for macOS), only once stage 3 is solid.

Stage 1 is § MIDI Out timing, listed with the engine work above. Stages 2–4 come after the downloadable macOS app.

## Not planned

Mirrored in the README's *Not planned* list, so nobody expects these. Some may come later if the capture workflow needs them, but none is on the roadmap.

- tempo or meter changes along the timeline (one tempo and one meter per project; the meter is stored as a list so changes could extend the format later, `060`)
- audio tracks and audio recording
- a built-in sampler or other built-in instrument: sampler and instrument plugins cover it, loaded through the plugin host (`130`)
- automation lanes and CC lane editing, and with them any view of recorded wheel data: pitch bend and the mod wheel stay invisible in the piano roll and the thumbnails until a lane shows them (decided 2026-10-14, `090`)
- mixing: built-in EQ, compression or other effects, effect plugins, sends, a mixer panel (Stev arranges, a DAW mixes: § Principle; the hand-off is a stem export)
- MIDI clock / Ableton Link / external sync
- notation
- whole-arrangement MIDI file export and multi-track import (one clip at a time each way, `060`)
- a richer browser: search, tags, favourites, preview playback, sample/preset categories (the browser has a Projects category of folders, `.stev` and `.mid` files, and a flat Plugins category)
- richer piano-roll editing: a draw/pencil mode, paint-drag, erase gestures, ⌥-drag duplicate, unsnapped drawing, drawing velocity in the velocity panel, Shift-click to add to the selection, drawing clips in the arranger
- more than 16 tracks, track reordering, track groups/folders
- DAW integration: Mackie Control or other control-surface sync (removed 2026-10-07; `archive/120-daw-track-sync.md`)

**Never in scope:** VST2 or any VST format older than VST3. The VST2 SDK is no longer licensed by Steinberg and cannot be redistributed under `MIT OR Apache-2.0`; only VST3 (MIT since SDK 3.8) and newer formats. See `180-vst3-host.md`.
