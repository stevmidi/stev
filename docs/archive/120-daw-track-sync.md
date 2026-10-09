# DAW Track-Select Sync (Mackie Control)

> **Removed 2026-10-07** (`240` § Trim). The code is gone: `midi/mcu.rs`, the `"mcu"` thread, `SequencerCommand::SelectTrackFromSurface` and the selection-origin handling. This file is kept, unmaintained, for its design: the codec and the two feedback-loop guards.

midi-seq mirrors its track selection to/from an external DAW (Logic Pro) by
emulating a **Mackie Control (MCU)** control surface. Change track in midi-seq
and the DAW selects the same track; select a track in the DAW and midi-seq
follows.

## Wiring

- **Port**: a dedicated always-on virtual input+output pair named
  `midi-seq Control` (`mcu::MCU_PORT_NAME`), registered by the `"mcu"` thread
  (`start_mcu_thread`, `src/core/threads/mcu.rs`). **Unix only** — virtual ports need
  CoreMIDI/ALSA. Excluded from midi-seq's own pickable port lists.
- **Outbound**: `select_track_workflow` → `apply_track_selection` →
  `EventHandlers::send_select_track_mcu` pushes bytes onto `mcu_out_tx`; the
  `"mcu"` thread forwards them out the virtual output.
- **Inbound**: the `"mcu"` thread's virtual-input callback runs
  `mcu::track_idx_from_message` and, on a hit, sends
  `SequencerCommand::SelectTrackFromSurface(idx)` and calls
  `repaint_ctx.request_repaint()` (a `midir` callback is not a window input
  event — see `000-architecture.md`).

## Protocol (`src/core/midi/mcu.rs`)

- Only the 8-strip `SELECT` subset of one Mackie bank (`BANK_STRIPS`) is
  implemented: track index `N` ↔ `SELECT`
  button `N` (`Note On` `0x18 + N`, channel 1, velocity `0x7F` press / `0x00`
  release). Each outbound selection is just that one tap — **no bank
  navigation**. Strip index is assumed to equal track index, i.e. the DAW's
  surface is on its first bank. Tracks 9 and up (a project holds up to
  `MAX_TRACKS`, 16) simply don't sync, either way — banking is not planned (`240` § D).
- Inbound: a lit `SELECT` LED (`Note On 0x18..=0x1F`, velocity > 0) means that
  track is now current. Velocity-0 (LED off) and every other MCU message
  (meters, faders, V-pots, timecode) decode to `None` and are ignored.

## Feedback-loop safety

Two guards, because the DAW echoes every `SELECT` midi-seq sends straight back
as an LED-state update:

1. **Origin suppression** — DAW-originated selection is applied with
   `TrackSelectOrigin::Surface`, which skips `send_select_track_mcu` so it
   isn't re-sent to the DAW.
2. **Echo window** — the `"mcu"` thread drops any inbound `SELECT` that lands
   within `mcu::ECHO_SUPPRESSION_MS` (200 ms) of its own last outbound byte.
   Without this, the DAW's echo of a midi-seq-side selection — or a stale
   intermediate `SELECT` from a fast A→B→C burst still in flight — was misread
   as a surface-side selection and snapped the UI back for a frame. The window
   only closes the DAW→midi-seq path while midi-seq is actively driving; a
   genuine surface selection made while midi-seq is idle is unaffected.

The `new_id == selected_track_id` early-return in `apply_track_selection` is a
third backstop: a same-track echo that slips through both guards is a no-op.

## DAW setup (Logic Pro)

1. Logic Pro → Settings → Control Surfaces → Setup → New → Install → **Mackie
   Control**.
2. Set both its **Input** and **Output** port to `midi-seq Control`.
3. Keep Logic's first 8 arrange tracks aligned with midi-seq's first 8 tracks,
   and the Mackie bank on the first bank.
   Hidden tracks, folder/track stacks, and Global-view shift MCU's track
   mapping — avoid them on the synced tracks.

## Non-Unix

`mcu_out_tx` still exists; its receiver is dropped and sends are silent no-ops.
No `"mcu"` thread is spawned. A Windows equivalent would need a loopMIDI-style
external virtual port.
