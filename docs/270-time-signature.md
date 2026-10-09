# 270 — Time Signature: Build Plan

**Status: in progress — phases 1–3 done (2026-10-09).** Branch `feature/time-signature`. The scope was decided in `240-release-plan.md` § Post-launch candidates (the first post-launch item); this file is how it gets built. When it lands, the as-built behaviour moves into the topic docs listed per phase and this file goes to `archive/`.

## Scope (decided 2026-10-09)

- **One meter per project.** Numerator 1–16 over a denominator of 4 or 8. A bar is `numerator × 4 / denominator` quarter notes; the tempo stays quarter notes per minute in every meter.
- **Stored as a list** of bar-keyed changes holding exactly one entry, so meter changes along the timeline can come later without a format break. Nothing reads past the first entry.
- **A meter change keeps ticks.** Notes, clips and the loop region stay put; bar lines, the grid, the ruler and the click's downbeat move. Undoable as one step.
- **UI:** a `4/4` chip beside the header's BPM chip; a double-click opens an inline field (Enter / Esc). No drag-scrub, no key binding.
- **Not in scope:** meter changes along the timeline; denominators other than 4 and 8; reading a meter from an imported SMF (the project's meter wins, as the project's tempo does).

## Design

### The `Meter` type (`core/time.rs`)

A small `Copy` value, `Meter { numerator: u8, denominator: u8 }`, built only through `Meter::new(n, d) -> Option<Meter>` (rejects anything outside the scope) plus `Meter::FOUR_FOUR` and `Default` = 4/4. It owns every piece of bar arithmetic:

- `bar_ticks()` — `PPQN × 4 × numerator / denominator` (3/4 = 2880, 6/8 = 2880, 7/8 = 3360). Exact for every supported meter since `PPQN` = 960.
- `beat_ticks()` — the *counted* beat, one denominator note: 960 for x/4, 480 for x/8. The click and the grid's beat lines use this.
- `bars_to_ticks(bars)`, `ticks_to_bars(ticks)`, `next_bar_boundary_after(tick)` — today's free functions, now methods.
- `bar_quarters_f64(bar)` — the quarter-note position of a bar's start, for what plugins are told.

The free `bars_to_ticks` / `bars_to_beats` / `ticks_to_bars` / `next_bar_boundary_after` go away, so the compiler finds every site that has to choose a meter. `bars_to_beats` has no direct replacement: its callers (zoom's default view, the plugin transport, the ruler test) mean *quarters*, and say so explicitly.

### Where the meter lives

- **Source of truth: one `Arc<AtomicU16>` in `SharedAtomics`** (`meter`, packed by `Meter::to_bits`), held by the `Sequencer` as it holds `tempo` (the atomic *is* the value, not a mirror of a field — `SharedAtomics`' module rule) and read and written through `Sequencer::meter` / `set_meter`. Read on the `"sequencer"` thread by capture, stopped capture, phrase detection, region windows and tempo fit, clip edits, quantize, the performance lane's bar jumps, the clip-export name, and the metronome (sequencer-thread-local).
- **Other threads read the same atomic**: the render thread (grid, ruler, zoom, scroll, overlays, the chip) and by the audio thread through `TransportState::snapshot` → `BlockTransport`. No lock, per the ground rules.
- **Persistence:** `ProjectData` gains `meter: Vec<MeterData>`, `MeterData { bar_index: i32, numerator: u8, denominator: u8 }` (zero-based bar), `#[serde(default)]` → `[4/4 at bar 0]`. Loading takes the first entry and falls back to 4/4 (with a `dprintln!`) if it fails `Meter::new`. The unsaved-changes fingerprint hashes `ProjectData`, so it covers the meter with no extra code. Stev 0.1.0 opening a newer file ignores the unknown field and plays it in 4/4 (no `deny_unknown_fields` anywhere).

### The behaviour decisions inside

- **The click** sounds every counted beat (`beat_ticks`), strong on the bar's downbeat: six clicks a bar in 6/8, three in 3/4. The metronome checks `tick % meter.beat_ticks()` itself on every tick instead of the clock's quarter-only `is_beat` flag; the clock stays meter-agnostic.
- **The grid ladder** stops being "a 256th to 64 bars, doubling". Below the bar it divides into counted beats, then halves each beat (the binary subdivisions of 960 or 480); above the bar it doubles in whole bars, as today. The **half-bar** snap tier only exists when the numerator is even (half a 7/8 bar is not on the beat grid). The invariant test "every tier divides the next coarser one" stays and grows meter cases.
- **Zoom stays in pixels per quarter.** The default arranger view keeps its 128 quarters wide (`BARS_IN_VIEWPORT × 4`, 32 bars of 4/4), so a meter change never refits or rescrolls the view (`220`'s rule: the view never auto-zooms). Fully zoomed out, the bar-coarsening rule is unchanged.
- **Capture and phrase detection** use bars of the project's meter: whole-bar new-clip windows, the phrase window (`PHRASE_DETECTION_WINDOW_BARS`), the preferred 1/2/4-bar spans, and Enter's tempo fit (whole bars of the meter). Risk: the pinned real-take fixtures are all 4/4. Non-4/4 detection gets synthetic tests only, and stays unverified on real playing until someone records a 3/4 or 6/8 take to pin.
- **The loop region and existing clips are never rewritten** on a meter change, so they may stop sitting on bar lines. That is the decided behaviour; nothing snaps.
- **Plugins:** the CLAP transport's `time_signature_numerator`/`denominator`, `bar_start` and `bar_number`; the VST3 `ProcessContext`'s `timeSigNumerator`/`Denominator` and `barPositionMusic` — all from `BlockTransport`, which now carries the meter.
- **SMF export** writes the project's meter: `FF 58 04 nn dd cc 08`, `dd` = log2(denominator), `cc` = MIDI clocks per click: 24 for x/4, 12 for x/8. Import keeps ignoring `FF 58`.

## Phases

One commit per phase on `feature/time-signature`. Phases 1–4 keep the app at 4/4 (no way to change it yet) but reach every site, each with non-4/4 tests, so every commit is safe to stop at. Each passes the full build gate.

1. [x] **`Meter` + state + persistence** (2026-10-09). The type and its tests (3/4, 6/8, 7/8, 4/4 round-trips; `Meter::new` rejects 0/x, 17/x, x/3). The `SharedAtomics` atomic and its `Sequencer` handle, the DTO field with the load fallback and its tests (missing field → 4/4; a 7/8 project round-trips; an invalid entry → 4/4). The free functions stay for now, as 4/4 wrappers over `Meter::FOUR_FOUR`. Docs: `060` (DTO), `000` (the module list). Only the methods something already calls exist yet; `beat_ticks` and `bar_quarters_f64` come with their first callers (phases 3–4).
2. [x] **Sequencer-thread sites** (2026-10-09). Retire the free functions in `core/` and `models/`: capture (`capture.rs`, the most sites), stopped capture, `region/` (window, tempo fit, phrase tokens), clip edits (retime, resize, import, commit), `cursor_region.rs`, quantize, `state.rs`, the performance lane, `storage.rs`'s export name, `config.rs` (`REGION_LENGTH_DEFAULT` becomes 2 bars of 4/4 by name). The model layer takes a `Meter` argument instead of reaching for a global. Tests: a 3/4 capture commit sizes to whole 3/4 bars; a 7/8 bar window; phrase spans in 6/8. Docs: `040`, `100`, `220` where they say "bar". As built: the free `next_bar_boundary_after` is gone; `bars_to_ticks` / `ticks_to_bars` / `bars_to_beats` stay as 4/4 wrappers only for the plugin transport (phase 3), the view (phase 4) and tests. Quantize and `state.rs` turned out to use bars only in tests. Two view-side reads came forward from phase 4, because they share model code with the sequencer: `Display` gained the `meter` handle and `Display::meter()`, for the clip view's scroll range (`reach_over` takes the meter, as the clip cursor's reach does) and for MIDI import, whose clip is built on the UI thread (`load_midi_clip` takes the meter). A 3/4 Enter test found `Clip::adjust_to_tempo`'s own whole-bar snap, which had been missed in the survey; it takes the meter now too. Capture fixtures gained a `meter` field (absent = 4/4), so a non-4/4 take can be pinned. The checked-in 4/4 takes all still pass unchanged.
3. [x] **The click, plugins, SMF export** (2026-10-09). Metronome on counted beats; `BlockTransport` carries the meter and drives CLAP and VST3; `write_smf` takes the meter. Tests: six clicks with one strong in 6/8; `bar_number` / `bar_start_beats` in 3/4; the `FF 58` bytes for 7/8. Docs: `130`, `180` (the transport section). As built: `ClockTick` lost its `is_beat` flag (the metronome was its only reader); `Metronome::on_tick` takes the meter from the pump (`sequencer.meter()`); `TransportState` holds the `meter` atomic and `snapshot` unpacks it into `BlockTransport::meter`. `Meter::bar_quarters_f64` has only plugin-host callers, so it carries the same non-macOS `allow(dead_code)` as `ticks_to_beats_f64`. 180 has no transport section; its `voice.rs` row names the meter instead. The free `bars_to_beats` / `bars_to_ticks` / `ticks_to_bars` are now left only for the view (phase 4) and tests.
4. **The view.** Grid ladder (above), the timeline ruler, zoom, scroll and overlays read the atomic. Tests: the ladder for 3/4, 6/8 and 7/8 (no half-bar tier in 7/8; every tier divides the next); the default view's width in quarters doesn't change with the meter. Docs: `030` (grid hierarchy, timeline).
5. **Changing it: `SetMeterEdit` + the chip.** The undoable edit beside `SetTempoEdit` (one step per typed value; undo restores the old meter, both writing the atomic), the `SequencerCommand` that carries it, the header chip and its inline field (parse `n/d`; invalid input leaves the field open and unchanged, Esc restores). Tests: the edit and its undo; the parser. Docs: `050` (the edit), `030` (header). `CHANGELOG.md` under `[Unreleased]`: the meter chip, and that project files gain a `meter` field older Stev versions ignore. Then `/simplify` over the branch, the gate again, and tick the item off in `240`.

## Open questions

- **The chip's input:** typed `7/8` only, or also a small popup of common meters (3/4, 4/4, 6/8)? Typed only is the plan; revisit after using it.
- **A non-4/4 phrase-detection fixture:** worth recording one real take in 3/4 or 6/8 before calling detection done in other meters, even if the author rarely plays in them.
