# Changelog

What changes between Stev's versions, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) as it applies before 1.0: a release
with a new feature (anything under **Added**) or a breaking change bumps the
minor version (0.1 → 0.2); a release of fixes only bumps the patch version
(0.1.0 → 0.1.1).

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

- Project folders can be made, renamed and deleted in the browser. ⌘⇧N (or the
  `+` that shows on the Projects row under the pointer) opens a name field for
  a new folder; nothing is made until Enter, and Esc makes nothing. ⌘R renames
  a folder as it renames a project. Backspace, then Enter, deletes a folder
  with its projects, moving it to the Trash; the prompt says how many projects
  go with it. Deleting the folder of the open project leaves the project open,
  and ⌘S saves it back, as after deleting the open project itself.

### Changed

- Save As now has a "Save in" dropdown to choose the folder the project goes
  in. It starts at the folder selected in the browser (or the one holding the
  selected project) while the browser is shown, else the open project's folder.
  Enter on a browser folder now only expands or collapses it; it no longer
  silently changes where new projects are saved.
- The browser's selected row is now plainly visible: a stronger accent fill
  while the browser has the keyboard, a grey fill otherwise. The open project
  is marked with a dot before its icon instead of a bar at the panel's edge,
  which looked like the selection; its folder is no longer marked.

## [0.6.0] - 2026-10-25

### Added

- A downloadable macOS app: each release from this one on carries
  `Stev-x.y.z-macos-universal.zip` (and its SHA-256), built by CI from the
  release's tag. It is unsigned, so the first launch needs System Settings →
  Privacy & Security → **Open Anyway**; the README's Download section has the
  steps. It is universal, but only tested on Intel so far.
- An **About** tab in Settings: the version, the licence, links to
  stevmidi.com and the repository, and the VST Compatible logo. On macOS it
  has **Check for updates**, which asks GitHub for the latest release when
  pressed (never on its own).
- The macOS app menu's **About Stev** opens the About tab, and it gains
  **Check for Updates…** and **Settings…** (`⌘,`).
- On macOS, double-clicking a `.stev` project in Finder, or dropping one on
  Stev's Dock icon, opens it (asking first if the open project has unsaved
  changes). A project from outside `Documents/Stev/Projects` is copied into
  it first; the original is left alone.
- A log file, `stev.log` in `~/Library/Logs/stev/`, with the previous run
  kept as `stev.1.log`. It records what a bug report needs, such as a plugin
  that won't load or a MIDI port that won't connect, and any crash. Settings
  → About → **Show log in Finder** shows it.

### Changed

- **Breaking** for forks: the `dprintln!` macro is gone. Stev logs through
  the `log` crate's macros (`debug!` for what `dprintln!` printed), and the
  `AGENTS.md` ground rule says so; `docs/080-conventions.md` § Logging has
  the levels.

### Fixed

- The Dock shows Stev's own icon while the app runs, not egui's default
  icon.
- Scanning VST3 plugins no longer flashes a second Stev icon in the Dock
  when a plugin (such as Kontakt 8) starts its own user interface toolkit
  while it is scanned.
- The clip view's ruler numbers bar 1 from the clip's window start, with the
  material before it counting down from 0. A captured clip showed bar
  numbers in the hundreds or thousands the longer a session ran.
- Enter's tempo fit while playing keeps playback in phase: the playhead moves
  with the retimed notes, so the clip plays on seamlessly instead of jumping
  to another point of the take until the next loop wrap. The same goes for
  `⌥=`/`⌥-` on a project's only clip, and for undoing or redoing either.
- The metronome no longer double-clicks when a tempo fit while playing
  switches it to the new tempo's beats.

## [0.5.0] - 2026-10-21

### Added

- **Rebind the commands in Settings → Keyboard (`?`).** Select a command
  and press Enter, or click its keys, then press the new key: it becomes
  the command's only key. A key another command has asks for a second
  press, and that command is left unbound (shown in red) until it gets a
  new one. Backspace unbinds the selected command, `⌘Backspace` puts its
  default keys back, and Reset all at the foot of the tab puts back every
  one. A dot marks a command off its defaults. Keys that can't be commands
  are refused with the reason in the footer: the clipboard, the system's
  `⌘Q` / `⌘H` / `⌘M`, `⌘,` and `?`, save and new project, and the arrows,
  Tab, Enter and Esc. Changes are saved at once in `settings.json`, only
  the ones you made, so a later version's new defaults still reach every
  command you never touched. A row whose key the computer keyboard plays
  while it is on says so. The search field and the list take turns at the
  keyboard: ↓, Enter or Tab moves into the list, and ↑ on the first row,
  ⇧Tab or a click on the field moves back; nothing is typed into the
  search while the list has it.

### Changed

- **`⇧Tab` steps back through the rows of Settings → MIDI**; it used to
  step forward, like `Tab`.
- **Commands go through one keymap** (`src/core/keymap.rs`), the groundwork
  for rebinding them in Settings (`docs/320-keyboard-shortcuts.md`). The
  default keys are unchanged, but a key now needs its exact modifiers: a
  stray one that used to be ignored is a different chord, so `⌘\`, `⇧Space`
  or `⌘⌫` (carve) no longer do anything. Layouts that type `\`, `/`, `[`,
  `]` or `+` with Shift, ⌥ or AltGr still reach them. While a plugin window
  has the keyboard, `.` now reaches the plugin; it was reserved for a Stop it
  hasn't been bound to for a long time.
- **The track header has a little more room at the top and bottom**, so the
  track name no longer sits against the lane's top edge. The shortest lane
  grows from 72 to 76 points to keep the whole header in it. Each track
  meter now sits the same distance from its lane's top and bottom edges in
  every lane, and so does each clip; both used to vary by a pixel or two
  from track to track.
- **Text fields leave room around the blinking caret.** The track rename
  field (`⌘R`) and the header's BPM and meter fields were barely taller
  than their text, so the caret ran their full height or past it; they now
  grow a little past the row or chip they open over, with the text where
  it was.

### Removed

- **The `?` help overlay is gone; `?` opens Settings → Keyboard instead.**
  The new tab lists every command with its keys, then the keys that aren't
  commands (the arrows, Enter and Esc, the clipboard, the mouse, the
  browser, the computer keyboard), with a search field that filters as you
  type ("loop", or "cmd+l"). The header's `?` chip opens it too.
- **`⇧F5` / `⇧F6` no longer step the colour theme.** Pick it in Settings →
  Appearance (`⌘,`).

### Fixed

- **Pressing ⌘, ⇧, ⌥ or ⌃ on its own no longer counts as a key press.** It
  closed a track's output menu, for one.
- **Keys typed with Shift no longer stop responding.** Letting go of Shift
  before the key itself (`?` typed as `⇧/`, `+` as `⇧=`, a Nordic `/` as
  `⇧7`) made the next few presses of it do nothing.
- **Text fields centre their text and caret**, and a field's hint (the
  track number in an unnamed track's rename field) is drawn in the field's
  own font, so an empty field's caret sits where a typed one does. Save As's
  name field is taller, so its caret no longer touches the field's edges.

## [0.4.0] - 2026-10-20

### Added

- **Export to audio (macOS).** `⌘⇧R` exports the arranger's time selection
  as WAV files: one per audible plugin track, at unity gain and centred, plus
  `Mix.wav`, all 32-bit float at the device rate and all the same length, in
  a new folder inside `Stev/Exports` named with the project, tempo, meter and
  start bar. The selection plays through once in real time; leave room in it
  for any tail you want. The click is never in the export. While it runs the
  footer shows how far it has got, `Esc` cancels it and removes its folder,
  and everything else waits ("Finish or cancel the export first"); the
  keyboard plays no plugin. The result warns when samples were dropped, the
  one glitch that damages the files, and notes any overrun or xrun in what
  you heard (the files take the samples before the device, so those don't
  reach them). An export whose audio device
  changes or goes away stops and says so.
- **Track meters.** A thin stereo meter runs down the right of each track
  header. On a plugin track (macOS) it shows the track's audio after its
  fader, pan and mute: the peak in a dim bar, the RMS level in a brighter one
  in front of it, and a peak-hold line that turns red when the track goes
  over 0 dBFS; the scale runs to +6 dB, with 0 dBFS marked a little below
  the top. On a MIDI Out track a single centred bar jumps to the velocity
  of each note the track sends, from a clip or from the keyboard on the
  selected track, and falls back; a muted or non-soloed track, or a muted note, leaves it empty.
  The meter grows with the lane, and the track header is 5 pixels wider to
  make room. The header's controls now sit at the top of the lane instead of
  centred, with the same small margin above the name and at both ends of the
  meter.

## [0.3.1] - 2026-10-14

### Changed

- **Changing a track's output can be undone.** Putting a plugin on a track,
  replacing one, removing one from the output menu and picking a MIDI channel
  are each one undo step. Undoing a replacement brings the old plugin back
  with its preset as it was, not as of the last save. A note sounding when
  the output changes is released on the output it leaves, so nothing hangs.
- **One clock timer on every platform.** The clock thread now waits for
  absolute deadlines with plain `Instant`s everywhere, replacing the
  macOS-only `mach_absolute_time` loop and the drifting sleep loop Linux and
  Windows used. Same timing on macOS; Linux and Windows should tick more
  steadily (unmeasured).

### Fixed

- **Quitting from the Dock or by logging out asks about unsaved changes.**
  Before, only ⌘Q and the window's close button did; the Dock's Quit and a
  logout, restart or shutdown quit at once and lost unsaved changes. A
  logout, restart or shutdown is now cancelled while Stev asks, even when
  nothing is unsaved, so you log out again once Stev has quit
  (`docs/060-persistence.md` § Unsaved changes).
- **MIDI Out notes leave on time.** Clip notes bound for external gear used
  to go out up to ~1.3 ms late, at random, even on an idle Mac. The output
  thread now times the last 2 ms before each note itself, so they're now
  within about 10 µs (`docs/160-midi-out-offset.md`).
- **Timing holds up on a busy machine.** The clock and MIDI Out threads now
  ask for real-time priority (macOS's time-constraint policy; best effort on
  Linux and Windows, unmeasured there). With every core of an Intel Mac busy,
  clock firings went from up to ~7 ms apart to within ~20 µs of 1 ms, and
  MIDI Out notes from up to ~4.5 ms late to under 20 µs.
- **Plugin rendering threads join the audio device's workgroup on macOS.**
  The threads that render plugins alongside the audio callback now tell macOS
  they share its deadline, which on Apple Silicon should keep them off the
  slower efficiency cores (untested there; neutral on Intel). The overrun
  journal's session line (`~/Library/Logs/stev/audio-overruns.log`) now says
  how many of them joined.

## [0.3.0] - 2026-10-13

### Added

- **Frame a take before it goes into a clip.** With the transport stopped,
  `\` (or `/`) into an existing clip no longer drops the detected phrase in
  at once: the clip view opens the take, and you choose which notes go in by
  moving its start and end like any clip's, with `[`/`]` or the band in the
  timeline strip. Notes come along whole by where they start (one played just
  ahead of the start too), and keep their timing against the grid. The clip
  you're inserting into already holds the chosen notes at its cursor, so you
  can loop the section and hear the take in place while you choose. Enter
  inserts it as one undo step; Esc (or ⌘Z, or leaving the clip view) cancels
  and keeps the take, so `\` opens it again. Other edits wait until you
  finish, and the footer says a take is open.
- **Drag a clip's start and end in the clip view.** The timeline strip above
  the piano roll shows the clip's window as a band in the clip's colour; drag
  either end of it to move the start or the end, snapped to the visible grid.
  The start works like `[`, the end like `]`. The band shows where the edge
  will land while you drag, and the change is made when you let go, as one
  undo step; Esc cancels the drag. While the project has one clip the loop
  follows.
- **Play notes on the computer keyboard.** ⌘K (Ctrl+K on Windows and Linux),
  or the piano chip at the right of the header, turns the letter keys into a
  piano for a machine with no MIDI keyboard: `A` `S` `D` `F` `G` `H` `J` `K`
  `L` are the white keys from C3, `W` `E` `T` `Y` `U` `O` `P` the black keys
  between them, `Z`/`X` move an octave down/up and `C`/`V` the velocity.
  The notes go wherever a MIDI keyboard's would — thru to MIDI Out and the
  armed track's plugin, the stopped `/` capture, live recording and the
  performance lane. While it is on, those keys play notes rather than their
  usual bindings (`K` no longer toggles the metronome, `T` no longer taps the
  tempo, `Z`/`X` no longer zoom); every chord with ⌘/Ctrl and every other key
  still works. The header chip lights up while it is on.
- **Icons in the browser.** Each row shows what it is at a glance: a folder
  (open while expanded), a project, a MIDI clip or a plugin.
- **Hover highlight in the browser.** The row under the pointer gets a faint
  background as the pointer moves through the list.
- **A button that shows and hides the browser.** A sidebar icon at the left
  end of the footer does what ⌘⌥B does. It stays in the same place whether
  the browser is open or not, and fills in while it is open.
- **Arrow keys in the unsaved-changes prompt.** ← and → move between Cancel,
  Don't Save and Save, and Enter presses the selected button (Save to start
  with).
- **Progress while a project opens.** Opening a project with plugins on its
  tracks shows a panel naming the plugin it is loading, how many are left
  ("3 of 8") and a progress bar, instead of a frozen window. The open project
  stays on screen, untouched, until the new one replaces it whole once every
  plugin is in. Input waits until then. A plugin the background scan hasn't
  found yet is waited for rather than skipped as not installed.
- **Tab moves the keyboard between the arranger and the clip view.** With
  the clip panel docked, Tab hands the keyboard to the other pane and leaves
  the layout alone, like a click in it. With the panel hidden or maximized it
  does what Shift+Tab does, since the other pane isn't on screen. With the
  browser panel focused, Tab hands the keyboard back to the pane you were in.
- **A Clips folder.** Exported clips (⌘⇧E) now go to one `Clips` folder
  beside the project folders, and the browser lists them under a CLIPS
  category of their own instead of mixed in with each folder's projects.
  `.mid` files exported before this stay where they are and no longer show
  in the browser. Move them into `Clips` to get them back.
- **The browser panel can be made wider.** Drag its right edge. The width is
  remembered between sessions.
- **The browser shows when it has the keyboard.** The accent edge that marks
  the focused pane now moves to the top of the browser panel while it has
  the keyboard, so only one edge is ever lit.

### Changed

- **The stopped `\` into a clip inserts the whole last phrase.** It used to
  insert at most the phrase's last bar, and closed the last note a tick early.
  Now `\` then Enter inserts every note of the detected phrase, each one
  whole; a note that runs past the clip's end is still cut there.
- **`]` in the clip view ends the clip exactly at the cursor.** With more than
  one clip in the project it used to round up to the next bar line; now it
  works like `[`, whatever the number of clips, and still stops at the next
  clip.
- **A new interface font, IBM Plex Sans.** It replaces egui's Ubuntu Light,
  which read too thin, across the whole app, and is drawn the way macOS draws
  text, so it looks as it does in Zed. Its licence is in
  `THIRD-PARTY-NOTICES.md`. The header's values are smaller, and the
  browser's text is a point larger, on taller rows.
- **More room in the browser.** The BROWSER heading is gone, so the list
  starts at the top of the window.
- **The footer runs under the browser panel.** The status bar now spans the
  whole window width and the browser panel stops above it, so the footer and
  its messages stay put when the panel shows or hides.

### Fixed

- **Moving a clip's start while it plays is heard at once.** It used to play
  the rest of that pass from the old point of the take, cut short, until the
  loop came round. The clip now switches to its new start straight away,
  without moving the playhead.
- **A clip's start can go back to the bar it began on.** After moving a clip's
  start past the silence before its first note, in the clip view, it couldn't
  be moved back to that bar line other than by undoing: the cursor and the
  start stopped at the first note. They now reach the bar line at or before
  the first note.
- **Text keeps its weight when macOS is in light mode.** Stev followed the
  system appearance for how it draws text, so in light mode its dark themes
  drew all text noticeably thinner. It now follows Stev's own theme, as do
  the text fields' cursor and selection colours and the window's title bar,
  which stays dark.
- **The browser scrolls smoothly.** Trackpad and wheel scrolling over the
  browser moves the list by the exact amount, like the arranger and clip view,
  instead of jumping a whole row at a time and dropping slow two-finger
  scrolls.

## [0.2.0] - 2026-10-10

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
- **Rename files in the browser.** ⌘R on a project or `.mid` row opens a
  name field over it: Enter renames the file, Esc cancels. A name that is
  already taken is refused. Renaming the open project makes ⌘S save under
  the new name.

### Fixed

- **The browser shows what you just saved.** A save, Save As or clip export
  while the browser panel is open now re-reads it, so the new project or
  `.mid` appears without hiding and reopening the panel.
- **The DSP chip keeps updating while stopped.** It used to freeze until
  the mouse or a key moved; it now refreshes four times a second, so the load
  from plugins played live stays visible.
- **The header chips no longer shift about.** The BPM, meter, position
  and DSP chips each keep a fixed width as their numbers change, so the row
  stays put instead of moving with every tick of the position readout.
- **Omnisphere's first note could come out distorted.** After the first
  Omnisphere load in a session, the first key played came with a short
  high-pitched burst over the sound. A silent VST3 instrument is now still
  called about ten times a second at 48 kHz, so work it does in the background keeps
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
