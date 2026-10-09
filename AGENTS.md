# Project Guidelines — Master Index

Stev (`stev`, formerly `midi-seq`) is a real-time MIDI sequencer written in Rust. It is a desktop application rendered with `egui`/`eframe` and driven by egui's native keyboard/mouse input. **macOS is the supported platform.** Linux and Windows build and pass the tests in CI but are otherwise untested, and the plugin host is macOS-only (`240` § B). It records, loops, and arranges MIDI clips across multiple tracks.

These guidelines are split into focused topic files. All of them live in [`docs/`](docs/).

## Ground rules (always apply — the topic files add detail, never override these)

- **Imports**: every type, trait, function, or macro is brought in with a `use` at the **top of the file** and referenced by its short name. Never write an inline multi-segment path like `crate::models::track::Track` or `super::super::config::X` in code. Never put a `use` inside a function body. (Short `std::` helper paths such as `std::mem::swap` / `std::iter::once` are the one accepted exception.) See `080-conventions.md` § Imports.
- **Debug logging**: `dprintln!`, never `println!` (compiles away in release).
- **Docs stay in sync**: when you change threading, command routing, view state, persistence DTOs, `EditResult`/`SequencerEdit` shapes, or a documented invariant, update the matching `NNN-*.md` file **in the same change**. A change a user would notice, or one that could break a project file or a fork, also adds a line to `CHANGELOG.md` under `[Unreleased]` (`080` § Changelog and Versions). A keybinding change also updates the help overlay's table (`HELP` in `src/view/display/help_overlay.rs`) beside `010-keybindings.md` — the table is hand-written, not generated from the matchers.
- **Tests in the same change**: adding or changing logic in a testable layer (`src/models/`, `core/time.rs`, `core/transport.rs`, pure `sequencer/` helpers, pure `view/` geometry) means writing/updating its `mod tests` in that same patch; a bug fix carries a regression test. See `000-architecture.md`.
- **Build gate before you're done**: `cargo check`, `cargo clippy --all-targets`, `cargo fmt --check`, `cargo test`, and `cargo doc --no-deps --document-private-items` must all pass clean — zero warnings, not just zero errors. Do not launch the app. CI runs the same gate on macOS, Linux and Windows after each push to `main` (toolchain pinned in `rust-toolchain.toml`). A change that touches `cfg`-gated code gets type-checked for the other platforms first (`080` § Agentic Editing). `cargo doc` catches the rustdoc links that a rename silently breaks; `--document-private-items` is required because almost everything here is `pub(crate)` and would otherwise go unchecked.
- **`/simplify` before merge**: before a feature/refactor/fix branch is merged to `main`, run `/simplify` over the branch's diff, apply what survives, and re-run the build gate. See `080-conventions.md` § Agentic Editing for what to skip.
- **No locks on the render path**; new cross-thread state is `Arc<AtomicX>` in `SharedAtomics` or a channel — see `000`/`080`.
- **Licensing**: the project is `MIT OR Apache-2.0`, headed for open source. Nothing may enter the repo that can't be redistributed under it: a new dependency needs a permissive licence (MIT/Apache/BSD/ISC/Zlib family; MPL-2.0 tolerated; never GPL/LGPL/AGPL-only; CI checks it with `cargo deny` against `deny.toml`; a `Cargo.lock` change also regenerates `THIRD-PARTY-NOTICES.md` with `cargo about`, which CI checks too), and no third-party samples, presets, fonts or images go in without a compatible licence committed beside them. See `080-conventions.md` § Licensing.

## Files

| # | File | Topic |
|---|------|-------|
| 000 | [000-architecture.md](docs/000-architecture.md) | Threading model, module layout, build/test, integration points |
| 010 | [010-keybindings.md](docs/010-keybindings.md) | Keyboard bindings: global chords, clip-view controls, Arranger F-keys, region controls, loop state |
| 020 | [020-views-and-state.md](docs/020-views-and-state.md) | ViewState transitions, layering principle, tracks/regions, reserved spans, event selection |
| 030 | [030-ui-design.md](docs/030-ui-design.md) | Theme, grid hierarchy, clip anatomy, panes (docked clip panel), arranger/clip-view zoom, playhead, timeline, rendering performance |
| 040 | [040-phrase-detection.md](docs/040-phrase-detection.md) | How the stopped `/` frames a take: capture source, scored phrase-start detection (pinned to real takes), new-clip and insert windows, Enter's tempo fit |
| 050 | [050-undo-redo.md](docs/050-undo-redo.md) | Undo architecture, EditResult, SequencerEdit, adding operations, invariants |
| 060 | [060-persistence.md](docs/060-persistence.md) | Project save/load, DTOs, new-project flows, tempo defaults |
| 070 | [070-quantization.md](docs/070-quantization.md) | Batch grid voting, strength ramp, swing detection, backward compat |
| 080 | [080-conventions.md](docs/080-conventions.md) | Coding conventions, naming, enums, imports, clippy, documentation, licensing (dependency / asset compatibility), agentic editing (incl. verifying macOS-only code from Linux) |
| 090 | [090-live-recording.md](docs/090-live-recording.md) | Dual-state design, tick handling, thumbnail snapshots |
| 100 | [100-running-capture.md](docs/100-running-capture.md) | Running capture window calculation invariants and regression guidance |
| 110 | [110-performance-lane.md](docs/110-performance-lane.md) | Arranger performance lane: live-only bar-jump triggering, arming, MIDI routing, cross-thread UI wake |
| 130 | [130-plugin-host.md](docs/130-plugin-host.md) | macOS-only per-track instrument plugin host: format-agnostic core (`InstrumentMixer`, `InstrumentVoice`/`InstrumentEditor`, `BlockTransport`, merged catalog, reclaim thread), CLAP module, picker, editors (`v`), persistence, caveats |
| 150 | [150-clock-position-sync.md](docs/150-clock-position-sync.md) | The two tick counters and the `clock_tick ≡ playback_tick (mod region_length)` invariant: `AlignToPlayback` (relative phase correction, never assignment), the `elapsed_ticks` odometer |
| 160 | [160-midi-out-offset.md](docs/160-midi-out-offset.md) | MIDI-output offset + `"midiout"` delay queue so external gear lands with the plugin/click paths |
| 170 | [170-multicore-scheduling.md](docs/170-multicore-scheduling.md) | `WorkerPool`: per-voice render across cores, the handshake that makes its unsafe sound, outstanding macOS audio-workgroup work, deferred look-ahead rendering |
| 180 | [180-vst3-host.md](docs/180-vst3-host.md) | macOS-only VST3 module of the plugin host: raw `vst3` bindings, out-of-process bundle scanning + mtime cache, COM load lifecycle, `Send` split, `ProcessData`, idle rule, `IPlugView` editor, state persistence, teardown order, MIDI/`IMidiMapping`. Read `130` first |
| 220 | [220-capture-without-pending-view.md](docs/220-capture-without-pending-view.md) | Capture as undoable edits (no `PendingClip`): the stopped `/` commit/insert, Enter's tempo fit, `[`/`]` edges, and the principles — edits never move the transport (except the one-clip loop), no stored mode-like state, the clip view never auto-zooms/scrolls. Open: adjusting the detected phrase after commit |
| 240 | [240-release-plan.md](docs/240-release-plan.md) | Pre-open-source plan: the principle (an opinionated, capture-first app), the features still needed, platforms (macOS first; CI on all three), packaging, and what's not planned |
| 250 | [250-first-feature.md](docs/250-first-feature.md) | Walkthrough for a first change: one real key press (`M`, mute notes) traced from `InputEvent` through `SequencerCommand`, the undoable `SequencerEdit` and `EditResult` to the UI, then the tests, docs and gate it needs; how to drive a coding agent through it and review the result; ends in a checklist. Start here before adding a feature |
| 260 | [260-porting.md](docs/260-porting.md) | Porting to Linux and Windows: what's macOS-only today (sound, timers, virtual port, file drag, quit), priorities (MIDI Out timing first), the `"midiout"` / clock timing analysis, the timing-probe design and fixes, a smoke test, rules for keeping a port behind `cfg` |

## Archived

`docs/archive/` holds topic files that are deliberately out of the index — completed design briefs and historical context that would only cost tokens. Their as-built behaviour lives in the indexed files above. Do not read them unless the user brings the topic up or you need the original design rationale a code comment points to. Currently:

- `archive/010-keypad.md` — the physical hardware keypad the bindings grew out of, its legends, the parked MIDI-CC controller route.
- `archive/120-daw-track-sync.md` — DAW track-select sync via Mackie Control emulation (the `midi-seq Control` virtual port, Logic Pro): **removed 2026-10-07**, with the author no longer running a DAW alongside. Kept for its feedback-loop and echo-window design if a DAW sync ever returns.
- `archive/140-device-frame-clock.md` — deferred, unimplemented proposal to credit musical time from the audio device frame count; its real trigger would be external-timeline sync.
- `archive/190-arranger-zoom.md`, `archive/200-clip-view-zoom.md` — zoom + adaptive grid design briefs (phases 1–3 built; as-built in `030`). Optional phase 4 never started.
- `archive/210-docked-clip-panel.md` — docked clip panel design brief (all phases built; as-built in `020` § Views and `030` § Panes).
- `archive/230-simplify-pass.md` — the completed whole-codebase `/simplify` pass (2026-09-30): per-area record and findings deliberately left.
- `archive/NNN-*-history.md` (`010`, `020`, `030`, `050`, `100`, `150`, `220`) — full snapshots of those docs taken 2026-09-30, before their phase narratives, bug stories, superseded designs and dated decisions were trimmed out. The live doc names its snapshot at the top where one exists. Not maintained.
