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

### Added

- **Time signature.** A `METER` chip beside the BPM chip in the header:
  double-click it and type a meter (`3/4`, `6/8`, `7/8` …, 1–16 beats over 4
  or 8), Enter to set it. One meter per project, undoable. The bar lines, the
  grid, the ruler, the position readout, the click (every counted beat,
  strong on the downbeat), capture's whole-bar windows, the plugin transport
  and exported MIDI files all follow it. Changing it moves no notes, clips or
  loop region, so they may stop sitting on bar lines. The tempo stays quarter
  notes per minute in every meter.
- Project files gain a `meter` field. Files without one open in 4/4; Stev
  0.1.0 opens a newer file with it and ignores it, playing it in 4/4.

### Fixed

- **Omnisphere's first note could come out distorted.** After the first
  Omnisphere load in a session, the first key played came with a short
  high-pitched burst over the sound. A silent VST3 instrument is now still
  called about ten times a second, so work it does in the background keeps
  moving instead of piling up for the next note.
- **Closing a plugin editor with its close button hid it before the plugin
  had finished.** A plugin that takes a while to close its interface, such as
  Kontakt, left Stev frozen for seconds with no window and no sign why. The
  window now stays up, with the busy cursor, until the plugin is done, as it
  already did when closing with `v`.
- **A VST3 editor showed only part of its interface when it opened.**
  Kontakt with a wide instrument such as Lunaris 2 stayed cut off until the
  mouse reached the window's right edge, and resizing the editor made its
  scrolling jitter. Stev now tells the plugin its new size immediately, as
  VST3 requires.
- **The click's downbeat drifted on a loop that isn't whole bars.** With a
  2½-bar loop the strong click landed off the bar line on every other pass,
  and starting playback could put the click off the beat. The click now
  counts from the playhead while playing.
- **The main window went black with a Native Instruments plugin loaded.**
  Kontakt 8, FM8 and other plugins whose editors draw with OpenGL left their
  own drawing context active, and Stev kept painting into it: the window
  flickered or went black and seemed frozen (⌘Q's save prompt opened unseen).
  Stev now takes its drawing context back every frame.
- **Quitting with a Native Instruments plugin loaded crashed.** With Kontakt 8
  or FM8 on a track, every quit ended in a segfault inside the plugin's own
  Qt shutdown code. Stev now exits without running loaded plugins' static
  teardown code.

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
