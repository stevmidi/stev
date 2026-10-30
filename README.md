# Stev

**Play freely, and keep what you just played.**

Stev is a capture-first MIDI sequencer for the desktop, written in Rust. It is
always listening: you never have to press record before you play. When
something is worth keeping, one key turns it into a clip, framed and in time,
whether the transport was running or not. Then you loop it, arrange it across
tracks, and play the next part on top.

It is an opinionated app, built around that one workflow, not a general-purpose
DAW. On macOS it hosts CLAP and VST® 3 instrument plugins on each track;
anywhere, it can drive a DAW or external gear over MIDI.

*A* stev *is a short Norwegian folk verse, traditionally improvised.*

Website: [stevmidi.com](https://stevmidi.com)

![Stev: six tracks of captured clips in the arranger, with the clip panel open on a mallet part](docs/images/arranger.png)

> **Status:** 0.12.0. Developed and tested on macOS, with a prebuilt app from
> 0.6.0 on ([Download](#download)). Linux and Windows build in CI and may work
> (MIDI Out only, no plugin host), but are untested; ports welcome.

## Download

Each release from 0.6.0 on carries the macOS app as
`Stev-x.y.z-macos-universal.zip`, under **Assets** on the
[Releases](https://github.com/stevmidi/stev/releases/latest) page. It needs
macOS 11 (Big Sur) or later.

Safari unzips the download for you; with another browser, double-click the zip
in Downloads. Then drag `Stev.app` into **Applications** in Finder before you
open it the first time. Opened from Downloads, macOS runs it from a temporary
read-only copy, and in Applications the next version replaces it in place.

The app isn't signed by Apple yet, so macOS blocks the first launch of each new
version with *“Stev.app” Not Opened*. To open it anyway (Apple's
[Open a Mac app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac)):

1. Try to open Stev once, and click **Done** in that dialog.

   <img src="docs/images/gatekeeper-not-opened.png" width="260" alt="The dialog: “Stev.app” Not Opened, with Done and Move to Bin">

2. Choose Apple menu > **System Settings**, then click **Privacy & Security**
   in the sidebar.
3. Under **Security**, next to *“Stev.app” was blocked to protect your Mac.*,
   click **Open Anyway**. The button is there for about an hour after the
   blocked launch.

   <img src="docs/images/gatekeeper-open-anyway.png" width="715" alt="Privacy & Security: under Security, “Stev.app” was blocked to protect your Mac, with an Open Anyway button">

4. Confirm, and enter your login password. Later launches of this version
   don't ask.

The screenshots are from macOS 15; other versions may look a little
different.

Each update asks again: the approval belongs to one build, and the next
version is a different one to macOS. If you're comfortable in Terminal, you can
skip the steps by clearing the download flag macOS put on the app, after moving
it to Applications:

```
xattr -dr com.apple.quarantine /Applications/Stev.app
```

That tells macOS you trust this download, as *Open Anyway* does, so only run it
on an app from the Releases page.

The app is universal, but only the Intel build has been tested so far. If you
run it on Apple Silicon, an issue saying whether it works is welcome. The first
time a project with plugins opens, macOS may ask whether Stev can find devices
on your local network. That request comes from a plugin, not from Stev, which
goes online only when you press **Check for updates** (Settings → About). Stev
works either way, though a plugin may need it for its licence or controller.

Each zip has a `.sha256` beside it; download both, and
`shasum -a 256 -c Stev-x.y.z-macos-universal.zip.sha256` checks the zip.

To build it yourself instead, see [Building](#building).

## Quickstart

Connect a MIDI keyboard and pick it under **Settings** (`⌘,`, the MIDI tab).
Then:

1. **Play.** Noodle until something sounds right. Nothing needs to be armed.
2. **`\`** commits the take (`/` works too, numpad included). Stopped, Stev
   finds the phrase you just played and makes it a clip, looping; while
   playing, it keeps the last loop pass ([Capture](#capture) has the rest).
   If the phrase came out too long or short, `[` and `]` move the clip's edges
   to the cursor.
3. **`Enter`** fits the tempo to that first clip, so it is a whole number of
   bars. There is no tempo to set up front: your first phrase is the tempo.
4. **`Space`** plays and stops. Play the next part over the loop, on the same
   track or another (`↑` `↓`), and commit again.
5. **`⌘Z`** undoes anything, a commit included.

**`?`** shows every key on one page. On Linux and Windows, `⌘` is `Ctrl`.

## Capture

Stev keeps everything you play on the MIDI input, whether the transport is
running or not, and `\` keeps your latest take. It works the same from the
arranger and from the clip view, stopped or playing. Where the take goes
depends on what is under the cursor on the selected track:

| | Empty space | A clip (or the clip view open on it) |
|---|---|---|
| **Stopped** | Stev finds the phrase you just played and makes it a new clip at the cursor. | The phrase goes into that clip, at the cursor. |
| **Playing** | The last loop pass becomes a new clip at the cursor. | The last loop pass goes into that clip. |

So you can noodle with the transport stopped and keep the phrase you liked,
or play over a running loop and keep the pass that worked, and either way
start a new clip or add to one you already have, such as a second hand on a
piano part. Every capture is one undo step.

The metronome (`K`) clicks with the transport stopped too: a steady pulse at
the project tempo, every beat the same, so you can play to a click before
anything is running. The accent on the first beat of the bar comes back once
the transport runs.

## Getting sound

Stev makes no sound of its own apart from the metronome (`K`). Each track sends
its notes to one of two places:

- **An instrument plugin** (macOS). Open the browser (`⌘⌥B`) and drag a CLAP or
  VST3 instrument from **Plugins** onto a track. `v` opens its editor.
- **MIDI Out**, to a DAW or a hardware synth. Pick the output port under
  **Settings**; each track sends on its own channel, which the chip on its
  header changes. On macOS and Linux, Stev also offers a virtual port,
  `Virtual: Stev`, for routing into other apps without any hardware.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/vst/VST_Compatible_Logo_Steinberg_negative.svg">
  <img src="docs/images/vst/VST_Compatible_Logo_Steinberg.svg" alt="VST Compatible" width="116">
</picture>

## AI agents (experimental)

An AI client you already use can read and edit the open project through
[MCP](https://modelcontextprotocol.io): marquee a section and ask "what key is
this in", "continue this with four bars" or "a bass line under this", or ask
"tidy up what I just played" before you commit it. Stev
sends nothing anywhere and needs no API key; your client does the thinking.
Each change it makes is one step `⌘Z` takes back, it never writes over a clip
unless you ask, and it leaves your selection alone. It's experimental: the
tools may change or go as real use shows what they're good for. Think of it
as help with writer's block or chores, not as the one making the music.

1. In Stev, turn on **Allow AI agents** under **Settings** (`⌘,`) → **AI**.
2. Add Stev as an MCP server to your client. The AI tab shows the Claude Code
   command for the copy of Stev you're running, and a click copies it.
   `--scope user` makes it work in every folder; without it, Claude Code
   finds Stev only in the folder you ran the command in.

   ```
   # Claude Code, once, in Terminal
   claude mcp add --scope user stev -- /Applications/Stev.app/Contents/MacOS/stev --mcp

   # Claude Desktop, claude_desktop_config.json
   { "mcpServers": { "stev": {
       "command": "/Applications/Stev.app/Contents/MacOS/stev", "args": ["--mcp"] } } }
   ```

   Any other MCP client: add a local (stdio) server with the command
   `/Applications/Stev.app/Contents/MacOS/stev` and the argument `--mcp`.
   Only Claude clients have been tested so far.

The client starts `stev --mcp` itself, which opens no window and talks only to
the Stev you have open, over a socket only your user account can reach. With
Stev closed or the setting off, every answer says so.

What the agent reads (notes, track and project names) goes to your AI client
and whoever runs its model, as anything else you ask it does. Track and
project names reach it as quoted text, but a project from someone else can
still carry instructions in them: take care with tools that act on your
computer.

## Not planned

Stev grows toward its capture workflow, not toward a full DAW. These are not on
the [roadmap](docs/280-roadmap.md):

- tempo or meter changes along the timeline (one tempo and one meter per
  project)
- detecting the meter from what you play: set it with the meter chip
- audio tracks and audio recording
- a built-in sampler or instrument: plugins cover it
- track automation lanes and controller-data editing
- mixing: EQ, compression, effect plugins, sends, a mixer panel. Stev arranges;
  a DAW mixes
- MIDI clock, Ableton Link or other external sync
- notation
- whole-arrangement MIDI file export and multi-track import (one clip at a time
  each way)
- a richer browser: search, tags, favourites, preview playback
- richer piano-roll editing: a draw mode, paint and erase gestures, drawing
  velocity
- more than 16 tracks, track reordering, track groups
- control-surface or DAW sync (Mackie Control and the like)
- VST2, ever: only VST3 and newer formats

Forks that take it somewhere else are welcome.

## Building

```bash
cargo run --release
```

The toolchain is pinned in `rust-toolchain.toml`, so `rustup` fetches the right
version on the first build. On macOS you need the Xcode Command Line Tools; on
Linux, the ALSA headers and `pkg-config` (`libasound2-dev pkg-config` on
Debian/Ubuntu).

## Working on the code

The topic docs under [`docs/`](docs/) cover the threading model, the timing
invariants, the conventions and the reasoning behind them. [`AGENTS.md`](AGENTS.md)
indexes them and sets the ground rules; it is written to be read by people and
coding agents alike (`CLAUDE.md` includes it for Claude Code). For a first
change, [`docs/250-first-feature.md`](docs/250-first-feature.md) follows one key
press from the keyboard to an undoable edit and lists what a feature needs.
[`CONTRIBUTING.md`](CONTRIBUTING.md) says what fits, how to report a bug and
what a pull request is checked against.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

## Trademarks

Forks are welcome, but a modified version needs its own name. Packaging Stev
unchanged, and saying a fork is "based on Stev", are fine.
[TRADEMARKS.md](TRADEMARKS.md) has the details.

VST is a registered trademark of Steinberg Media Technologies GmbH. The VST
Compatible logo is used under Steinberg's
[usage guidelines](https://steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/Usage+guidelines.html)
(`docs/images/vst/`).
