# Quantizer

Quantization logic lives in `src/models/clip/quantize.rs`.

## Key Design Points

- **Batch grid voting**: straight 16th (240 ticks at PPQN=960), triplet 16th (160 ticks), and swung grid are all evaluated by summing total distances across **all** candidate NoteOn events. The grid with the smallest total distance wins for the entire batch — individual notes never split to different grids within a single quantize call.
- **Smooth strength ramp** (`quantize_strength`): pull is 0 inside `QUANTIZE_TOLERANCE_TICKS` (dead zone), then ramps linearly to `QUANTIZE_LERP_FACTOR` at half a grid step. This eliminates the hard cliff where one tick past the dead zone previously triggered full-strength snap.
- **Grid anchored to tick 0**: the grid is aligned to clip-tick 0 (natural beat boundary), not to `region.start`, so notes on absolute beats are recognised even when the region starts off-beat (e.g. a clip whose start was trimmed).
- **The operand is the clip view's selection, or the whole clip window**: `Q` is bound only in `Clip` (`input_handler.rs`'s bare `Key::Q` arm → `SequencerCommand::QuantizeEvents` → `QuantizeEventsEdit`, undoable, the selection kept). `Clip::quantize()` votes its grid over the selected `NoteOn`s; with nothing selected, over every `NoteOn` inside the region window `[region.start, region.end)`. Material kept outside the window (`220-capture-without-pending-view.md`) is never a candidate with nothing selected — it doesn't play, so it must not sway the vote — but a selected note outside the window is quantized. The candidate set bounds which notes move, not where they may land. **There is no quantize in the Arranger**: the old marquee `Q` (`QuantizeInRangeEdit`, quantizing across every clip under a marquee) was removed — keeping track of what a marquee over several clips would move got too complicated. Do not reintroduce it.

- **Notes only**: quantize moves `NoteOn`/`NoteOff` pairs and leaves every other event — a recorded pitch bend or mod-wheel move (`090`) — where it was played. Without a controller lane to see them that is the honest choice; a fork adding lanes decides whether they follow.

## Swing

- **Swing-aware grid**: a clip's `swing_pct` field (50–75, stored in `Clip` and serialized in `ClipData`) defines a per-beat two-position grid where on-beats sit at `k × 960` and off-beats at `240 + swing_shift` and `720 + swing_shift` (where `swing_shift = (swing_pct/50 − 1) × 240`). When `swing_pct == 50` the swung grid is skipped (identical to straight).
- **Automatic swing detection** (`detect_and_store_swing`): called at clip commit time in `build_committed_capture_clip` (running capture), `build_stopped_capture_clip` (stopped capture; it also counts the notes kept outside the window, whose event space is rebased so the window starts on a bar line). It classifies NoteOn events whose nearest straight 16th is one of the two off-beat positions within a beat, averages their timing offset, and stores the result as `swing_pct`. Requires ≥ 2 off-beat samples; otherwise defaults to 50. The capture inserts into an existing clip (`InsertCaptureEdit`, running or stopped) do **not** call it — the target clip's existing `swing_pct` is preserved.
- **`swing_pct` field placement**: belongs in the "MIDI events, region and swing" group of `Clip`, not the playback state group.
- **`ClipData` backward compatibility**: `swing_pct` uses `#[serde(default = "default_swing_pct")]` so existing `.stev` files without the field load cleanly as 50.
