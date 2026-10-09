# Contributing to Stev

Thanks for looking. Stev is an opinionated app: a capture-first MIDI sequencer
that its author builds to use. Play freely, keep what you just played, loop it,
arrange it. Contributions that make that workflow better are welcome. Ones that
take the app somewhere else are better as a fork, and the licence and the docs
are there to make forking easy.

It's a one-person project, so replies can take a while.

## What fits

**Welcome, as a pull request:**

- Bug fixes, with a regression test that fails without the fix.
- Small improvements to something that already exists: a clearer message, a
  smoother edge case, a faster path.
- Timing and correctness work: notes landing late, hanging, or out of sync.
- Fixes to the docs, including the agent-oriented ones under `docs/`.
- Linux and Windows fixes. They build in CI but are untested (see below).

**Open an issue first:**

- A new feature, a new key binding, or a change to how an existing one behaves.
- A new dependency.
- Anything that touches the project file format (`.stev`).

A feature is judged by whether it serves the capture workflow, not by whether
it would be useful to someone. Some will be declined for that reason alone,
however well they're built. Talking it through in an issue first saves you
writing code that won't be merged. The README's
[Not planned](README.md#not-planned) list covers what's already been decided
against; VST® 2 support is never in scope. (VST is a registered trademark of
Steinberg Media Technologies GmbH.)

## Reporting a bug

Open an issue with:

- What you did, what you expected, and what happened instead. Steps that
  reproduce it are worth more than anything else.
- Your macOS version and Mac (Intel or Apple Silicon).
- Your MIDI keyboard or interface, and where the sound goes: which plugin and
  its format (CLAP or VST3), or which MIDI Out device or DAW.
- For audio dropouts or crackle: the tail of
  `~/Library/Logs/stev/audio-overruns.log`, which records each overrun with
  what took the time.
- If it depends on a particular project, the `.stev` file, as long as it holds
  nothing you'd rather not share.

On Linux or Windows, say so, and include the distribution or Windows version.

## Writing the code

Everything you need is in the repo:

- [`AGENTS.md`](AGENTS.md) sets the ground rules and indexes the topic docs. It
  is written for people and coding agents alike.
- [`docs/250-first-feature.md`](docs/250-first-feature.md) follows one key
  press from the keyboard to an undoable edit, then its tests and docs, and ends
  in a checklist. Start there.

The rules a pull request is checked against, in short:

- **Tests in the same change** as the logic, and a regression test with every
  bug fix.
- **Docs in the same change**: if the code changes something a `docs/` file
  describes, that file changes too. A new or changed key also updates
  `docs/010-keybindings.md` and the help overlay (`?`), and anything a user
  would notice gets a line in [`CHANGELOG.md`](CHANGELOG.md) under
  `[Unreleased]`.
- **The build gate passes with zero warnings**: `cargo check`,
  `cargo clippy --all-targets`, `cargo fmt --check`, `cargo test` and
  `cargo doc --no-deps --document-private-items`. CI runs the same gate on
  macOS, Linux and Windows. The toolchain is pinned in `rust-toolchain.toml`.
- **Try it in the app.** Tests can't tell you how a change feels to play.

Coding agents are fine to use; much of Stev was written with one. Point the
agent at `docs/250-first-feature.md`, and read its diff and try the result
before you open the pull request: you're the one vouching for it.

Keep a pull request to one change. `main` has a linear history, so it may be
rebased or squashed when it's merged.

### Linux and Windows

macOS is the supported platform. Linux and Windows build and pass the tests in
CI, but nobody has checked that they play correctly, and the plugin host is
macOS-only, so there a track can only send MIDI Out. Ports are welcome.

The most urgent need is **MIDI Out timing**: with no plugin host, it's the only
way Stev makes a sound off macOS. The clock loop there is less precise than
the macOS one, and on Windows the MIDI output wait follows the 15.6 ms system
timer tick, which could mean up to ~15 ms of jitter per note.
[`docs/260-porting.md`](docs/260-porting.md) lists everything that's macOS-only
today, the priorities, how to measure timing before and after a fix, and a
smoke test. Keep platform code behind `cfg(target_os = …)` and out of the
shared code, so a port stays a port.

## Licensing

Stev is licensed under MIT or Apache-2.0, at your option. Anything you submit
is licensed the same way, with no extra terms (see the README's
[Contribution](README.md#contribution) section). That means:

- A new dependency needs a permissive licence (MIT, Apache, BSD, ISC, Zlib and
  similar; MPL-2.0 is tolerated). `cargo deny check licenses` checks it, and CI
  runs it. If you change `Cargo.lock`, regenerate `THIRD-PARTY-NOTICES.md` with
  `cargo about generate about.hbs -o THIRD-PARTY-NOTICES.md`.
- No third-party samples, presets, fonts or images unless their licence allows
  redistribution and is committed beside them.
- Only submit work you have the right to submit under these terms.
