# Changelog

What changes between Stev's versions, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) as it applies before 1.0: a change
that breaks something bumps the minor version (0.1 → 0.2), anything else the
patch version (0.1.0 → 0.1.1).

**Breaking** means one of:

- **A project file stops loading the same way**: a `.stev` saved by an earlier
  version no longer opens, or opens with something lost or changed. New fields
  that older files simply don't have are not breaking; renamed or removed
  fields are (`docs/060-persistence.md`).
- **A change a fork has to deal with**: a large rename or module move, or a new
  ground rule in `AGENTS.md`.

Breaking entries are marked **Breaking** and say what to do about them.

## [Unreleased]

## [0.1.0] - 2026-10-09

The first public release. Stev is a capture-first MIDI sequencer for macOS:
play freely, and keep what you just played.

### Added

- **Capture without a record button.** Stev keeps listening to the MIDI input.
  `\` (or `/`) turns what you just played into a clip, from the arranger or
  the clip view: stopped, it finds the phrase you played; running, it keeps the
  last loop pass. With the cursor on a clip, the capture goes into that clip. `[` and `]` move a clip's edges,
  and `Enter` fits the tempo to the project's first clip.
- **Live recording** onto a track with `R`, alongside capture.
- **An arranger** with up to 16 tracks: add, remove and rename tracks, a colour
  per track, volume, pan, mute and solo, a loop region, zoom, and a marquee for
  range edits: split, delete, delete time, insert silence, duplicate, move,
  merge clips, and mute a range.
- **A clip panel** docked under the arranger, with a piano roll: select,
  move, resize, transpose, nudge, mute, delete, copy, cut, paste and duplicate
  notes, change velocity (⌘-drag), and quantize with swing detection.
- **Pitch bend and mod wheel** recorded with the notes.
- **Undo and redo** for every edit, capture included.
- **Instrument plugins on macOS**: a CLAP or VST® 3 instrument per track,
  with its editor and its state saved in the project.
- **MIDI Out** to a DAW or hardware, a channel per track, with an output offset
  to line external gear up with the plugins and the click, and a virtual
  `Virtual: Stev` port on macOS and Linux.
- **A browser panel** for projects, `.mid` files and plugins.
- **MIDI files, one clip at a time**: export a clip to a `.mid`, and import a
  `.mid` as a clip by dragging it in from the browser or the file manager.
- **Projects** saved as `.stev` files in `~/Documents/Stev`, with names and a
  prompt before losing unsaved changes.
- **Tempo** typed, dragged or tapped (`T`), and a built-in metronome (`K`)
  that also clicks while the transport is stopped.
- **A performance lane** above the tracks that turns the MIDI keyboard into
  bar-jump triggers while playing live.
- **Settings** (`⌘,`) for MIDI ports and appearance, and a keyboard help
  overlay (`?`).

### Platforms

Developed and tested on macOS. Linux and Windows build and pass the tests in
CI but are untested, and have MIDI Out only: no plugin host
(`docs/260-porting.md`).

---

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/vst/VST_Compatible_Logo_Steinberg_negative.svg">
  <img src="docs/images/vst/VST_Compatible_Logo_Steinberg.svg" alt="VST Compatible" width="116">
</picture>

VST is a registered trademark of Steinberg Media Technologies GmbH.
