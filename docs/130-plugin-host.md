# Instrument Plugin Host (macOS)

`src/core/plugin_host/` is a **macOS-only** host for **instrument** plugins: one
plugin per sequencer track, chosen from the browser panel's Plugins category, each with its own
editor window. Everything in the module is behind
`#[cfg(target_os = "macos")]`; other platforms compile with the MIDI-feed
receivers dropped.

## Format-agnostic core, one submodule per format

The module is split so that **nothing above the format submodule names a plugin
format**. The core owns the audio path, the scheduler, the transport, the
catalog, the editor window and the key guard; a format supplies two traits and
one `load` function.

| Trait | Thread | Implemented by |
|---|---|---|
| `voice::InstrumentVoice` (`Send`) | audio callback / `WorkerPool` worker | `clap::ClapVoice` |
| `editor::InstrumentEditor` (`!Send`) | eframe main thread | `clap::ClapEditor` |

`InstrumentVoice` is `queue_midi` / `render_block` / `sample` / `clear_events`
plus `mix()` / `mix_mut()`, which expose the `VoiceMix` the mixer owns on every
voice: the `sleeping` idle flag, the per-block `rendered` flag, and the
`gain_l` / `gain_r` ramp endpoints. `InstrumentEditor` is `show` / `close` /
`toggle` / `teardown_gui` / `save_state` / `deactivate` / `pump`.

`catalog::PluginFormat` is the discriminant. It is read in exactly **one**
place — `load_instrument` in `mod.rs`, which dispatches to that format's `load`
and gets back a `LoadedInstrument` (the boxed voice + boxed editor pair).
Everything downstream works through the traits. `PluginFormat` also carries the
format's bundle extension and install roots, which is all the shared catalog
walk needs.

Both `clap/` and `vst3/` are fully hosted; see `180-vst3-host.md` for what VST3
does differently. Neither format required a change
to the mixer, and the VST3 editor reuses `window.rs` and `key_guard.rs`
unchanged — everything in this file's layer has held across both formats.

**Design note - one mixer, not one per format.** A second `AudioSource` per
format was considered and rejected: it would duplicate the pending-event
scheduler, the gain ramp, mute/solo and the transport translation, run two
`WorkerPool` fan-outs per block (worse load balancing), and leave two arrays
keyed by the same track index free to disagree about who owns a track.

**Slots, not positions.** Every per-track index on this side — the mixer's
voices and gain caches, the `TrackMixAtomics` it reads, `InstrumentNotes`, the
`track` on every `ClipInstrumentEvent` / live-thru event / `PluginHostCommand`,
`live_instrument_target`, `Display`'s `track_instruments` and the reclaim ack —
is the track's **engine slot** (`Track::slot`), which it keeps for its whole
life however tracks above it are added or removed. So a track add / remove
moves nothing in the mixer; see § Track add / remove. `Display`'s plugin
methods take a track's position and look the slot up in its `tracks` mirror
(`TrackLane::slot`).

## What it does

- **No own `cpal` stream.** `start_plugin_host` (called from `main`) builds an
  **empty** `InstrumentMixer` and hands it to the running `"audio-engine"`
  (`src/core/audio/`) as a second `AudioSource` via `EngineHandle::add_source`.
  The engine's one output stream sums `MetronomeSource` + `InstrumentMixer` over one
  shared `AudioClock`; `InstrumentMixer::render_into` is called once per engine
  sub-block (`frames` ≤ the engine's `MAX_FRAMES`, a valid single process
  block for every hosted format — no internal split). `start_plugin_host` also spawns a `"plugin-host"` thread, but
  it does **only** off-audio-thread voice reclamation (see shutdown below), not
  audio.
- **`"plugin-catalog-scan"` thread** (`start_plugin_catalog_scan`, called from
  `main` right after `start_plugin_host` succeeds): runs `catalog::scan_catalog()`
  once — every format's scan, merged — — loading every installed bundle's dylib just to read its metadata —
  and sends the result back on a `crossbeam_channel`. `Display::pump_instrument_editors`
  polls it every frame and stores the first result into `Display::plugin_catalog`,
  so the browser's first show never blocks the eframe main thread on this
  scan. See Channels below.
- **Buffer size** is the audio engine's concern now (`src/core/audio/engine.rs`):
  it requests `DESIRED_BUFFER_FRAMES` (256) as `cpal::BufferSize::Fixed`, clamped
  by `pick_buffer_size` to the device's range and probe-tested with
  `probe_buffer_size_supported` (a throwaway no-op stream), falling back to the
  device default if rejected. `SCHEDULE_DELAY_FRAMES` (256, in `core/audio/mod.rs`)
  is the constant clip-event scheduling delay — roughly one buffer.
- **Per-track plugins.** From the browser panel's **Plugins** category (Enter
  on the selected track, or a drag onto any track — `Display::put_plugin_on_track`,
  `020` § Views) the user puts a plugin on a track, setting its output to
  `Instrument`; the output menu on the track header (`020` § Track Header
  Output Chip & Menu) removes it again or picks a MIDI channel instead. `InstrumentHost::track_instruments`
  keeps each track's editor together with the catalog entry it came from (`TrackInstrument`), so the browser can
  skip a pick the track already has and name the plugin it replaces. `Display::load_slot_instrument`
  runs the format's `load` on the main thread (the `!Send` plugin instance stays with
  the UI as an `InstrumentEditor`), builds the matching `InstrumentVoice` from
  the `Send` processor, and sends `PluginHostCommand::Insert { track, voice }`
  to the mixer. `load_instrument` picks the format from the catalog entry; the
  `Display` side never names one.
- **`InstrumentMixer`** (`mixer.rs`) owns `[Option<Box<dyn InstrumentVoice>>; MAX_TRACKS]` — sized to the track capacity, not the live count, so adding a track never reallocates on the audio thread.
  Each callback it drains `cmd_rx` (insert / remove), `clip_midi_rx` and
  `live_midi_rx` (`(track, bytes)` → `voices[track]`), then runs **two passes**
  over the voices:
  1. a **render pass** — each loaded, non-sleeping voice's `process()` into its
     own `out_bufs` (one `[channel]` group per output port the plugin declares;
     the mixer later reads only port 0), recording on the voice whether the
     block produced usable audio (`VoiceMix::rendered`: false for a sleeping
     voice, never called into, and for a plugin whose `process()` failed).
     Every iteration touches only
     its own voice and reads nothing another voice writes, so it runs through
     `RenderCtx::pool` (`WorkerPool::for_each`) — spread across the engine's
     worker threads *and* the callback thread, sized from the **awake** voice
     count so one busy voice runs inline and never pays to wake anyone.
     `process()` on a worker thread is legal CLAP: `[audio-thread]` forbids
     concurrent calls on one *instance*, not a different thread per instance.
     See `170-multicore-scheduling.md`.
  2. a **summing pass** — the per-track gain ramp accumulated into the shared
     mix, in track order.

  The split is deliberate. The render pass carries essentially all the cost and
  is embarrassingly parallel (`Instrument` tracks are independent — no buses,
  sends or sidechains), while the summing pass writes one shared accumulator and
  is a couple of multiply-adds per frame per track. Keeping them apart is what
  lets the render pass be handed to worker threads later without touching the
  mix logic. Note that `HostShutdown.in_process` is set around the *whole* of
  `render_into`, so it still means "no `process()` call is in flight" — any
  future worker pool must never let `render_into` return before every voice's
  render has completed. Clip and
  live-keyboard events are treated identically once received — the mixer
  never distinguishes their source, it just has two rings to drain instead of
  one (see Channels below for why they can't share a ring).
- **Per-track volume / pan.** As each voice is summed into the mix, the mixer
  multiplies its stereo output by a per-channel linear gain derived from
  `SharedAtomics.track_mix` (`TrackMixAtomics` — `volume_db` + `pan` as `f32`
  bit patterns, written by the sequencer thread's `SetTrackVolume`/`SetTrackPan`
  handlers). `mix::gains_for` (`core::audio::mix`) turns `(dB, pan)` into
  `(gain_l, gain_r)` — a **balance** law, not equal-power: centre is `(1.0,
  1.0)`, hard-one-side mutes the other. The mixer caches the result per track
  and only re-runs the `powf` when the atomic bits change. Each block ramps
  linearly from the voice's last gain to the new target (`VoiceMix::gain_l` /
  `VoiceMix::gain_r`), so a fader move doesn't zipper; a freshly loaded voice starts at
  `0.0` and fades in over one buffer. A sleeping / failed-to-process voice
  snaps its stored gain to the target (it contributed silence, so there's
  nothing to ramp). The arranger draws the matching bars — see
  `030-ui-design.md` § Arranger Track Header.
- **Per-track mute / solo cut the tail, not just the notes.** `target_gains`
  also reads `track_mix.mute` / `track_mix.solo` (`AtomicBool`s, fresh every
  block — no `powf`, so no caching needed) and, when
  `mix::track_is_audible(muted, soloed, any_solo)` is false, forces that
  track's target gain to `(0.0, 0.0)`. The sequencer already stops a muted
  track's *new* notes in `Sequencer::tick`; zeroing the mixer output on top of
  that is what silences a reverb / delay tail the plugin is still generating
  internally. The existing per-block ramp fades it to zero over one buffer, so
  the mute click-free; un-muting ramps back up (a still-ringing tail resumes,
  which is acceptable). Not undoable, solo not persisted — see
  `020-views-and-state.md` § Track Header Mute / Solo Buttons.
- **Idle voices are skipped.** How a voice decides it is idle is the format's
  business; the shared flag is `VoiceMix::sleeping`. `ClapVoice::render_block`
  checks the `ProcessStatus` a plugin's `process()` call returns; `Sleep` (CLAP's
  "no more processing is required until the next event or variation in audio
  input") sets `VoiceMix::sleeping`, and `InstrumentMixer::render_into` skips
  `render_block` entirely for a sleeping voice on later callbacks — its
  contribution to the mix is correctly zero without ever calling into the
  plugin. `queue_midi` clears `sleeping` on any new event (clip, live, or the
  note-reset burst), waking the voice again. A CLAP plugin can also wake
  itself: `request_process` (e.g. after a knob turned in its own editor) sets
  a flag on `ClapHostShared`, and the mixer calls
  `InstrumentVoice::wake_on_request` on every voice before the render pass,
  which takes the flag and clears `sleeping`. VST3 has no such request; its
  `wake_on_request` wakes the voice while UI parameter changes wait in its
  ring (see `180` § The parameter bridge). `request_restart` is still ignored.
  So idle CPU no longer scales with the number of *loaded* synths, only with
  how many are actually doing something.
- **Live-MIDI target.** `Display::set_live_instrument_target` writes the target track's
  slot into `SharedAtomics.live_instrument_target` (`-1` = none) whenever the
  selected track changes to one that has a plugin loaded (or stops being one),
  so playing the keyboard sounds the selected track's instrument — like a
  DAW's record-armed track. `MidiInputForwarder` (a different thread) reads
  that atomic to tag each live-thru event with the right slot before
  pushing it onto its own ring. No `PluginHostCommand` round-trip is involved.
  A held note's note-off is tagged with whichever track its note-on used, not
  the current value of the atomic (`MidiInputForwarder`'s `held_live_targets` /
  `route_held`, which `route_channel` also uses for `held_note_channels` on
  MIDI-OUT) — otherwise switching the armed track mid-hold sends the note-off
  to the new track's plugin and strands the note sounding on the old one.
- **Clip playback → plugin.** `TrackOutput::Instrument(InstrumentRef)` (in
  `models::track`) routes a track's clip events into `Sequencer`'s
  `instrument_midi_tx` ring as a `ClipInstrumentEvent { track, message, when }`
  (rewritten to MIDI channel 0). `message` is a `Midi3` (`[u8; 3]`, from
  `core::midi::message`) — inline, so nothing crossing the ring to the audio
  thread has a heap payload to free there. `Sequencer::tick(at)` tags each with
  `EventTime::At(at)` — the intended `Instant` of the tick that produced it,
  carried from `Clock` (see Sample-accurate scheduling below). `chase_notes()`
  and `release_instrument_notes()` use `EventTime::Immediate` (a seek / stop has
  no meaningful future position — sound it at the next block, but never ahead
  of that track's already-queued `At` events; see the scheduling step below).
  `Sequencer::preview_note` (click-to-select, the event marquee's newly
  joined notes, transpose, the piano roll's inserted note) pushes its audition
  here too on a plugin track as an `At` pair — note-on `At(now)`, note-off
  `At(now + PITCH_PREVIEW_MS)` — held in the mixer's `pending` until due.
  Never an `Immediate` note-on: it would queue behind the previous preview's
  pending note-off, so a quick marquee sweep or pitch drag heard its auditions
  late and some squeezed to nothing.
- **CLAP transport.** Each processed block gets a `TransportEvent` built from
  `SharedAtomics` (`TransportState` in `mixer.rs`): tempo (exact, from the
  `tempo` µs-per-quarter atomic), playhead (`song_pos_beats` /`bar_number`,
  tick-granular — `playback_tick` only advances once per sequencer tick),
  `IS_PLAYING`, the loop region as `IS_LOOP_ACTIVE` + `loop_*_beats`, and the
  project's meter (`time_signature_*`, with `bar_number` / `bar_start` in its
  bars; positions stay in quarter notes in every meter). So a plugin's
  tempo-synced arpeggiator / delay / LFO follows the sequencer and start/stop.
> All the plugin-host fields on the `Display` side are grouped into one
> `InstrumentHost` struct (`view/display/instrument.rs`), held as
> `Display::instruments` behind `#[cfg(target_os = "macos")]`. The methods that
> drive it stay on `Display` (same module) and reach in through
> `self.instruments`. Names like `Display::track_instruments` /
> `Display::plugin_catalog` below are shorthand for
> `self.instruments.track_instruments` / `self.instruments.plugin_catalog`.

- **Editors.** Each `ClapEditor` (behind a `Box<dyn InstrumentEditor>`) holds a `!Send` `PluginInstance` and lives in
  `Display::track_instruments[track]` (a `TrackInstrument`, beside its catalog entry). It is pumped once per frame from
  `Display::logic` (via `pump_instrument_editors` — `logic` rather than `ui` so it
  keeps running while the main window is minimised), which schedules the next repaint
  at the shortest plugin timer cadence across all open editors. `v` in the
  Arranger toggles the selected track's editor window (`InstrumentEditor::toggle`).
  A plugin picked in the browser opens its editor immediately; plugins restored on
  project load come up with **no editor** (the GUI is only ever built on the
  first `show`). The editor window is brought up with `orderFront`
  (not `makeKeyAndOrderFront`) so the main window keeps keyboard focus and `v`
  keeps toggling — see the caveat about focus below. `pump` reconciles
  `ClapEditor::visible` with the native window each frame (`PluginWindow::is_visible`),
  so closing the window with its title-bar button is noticed and `v` reopens it.
- **Closing an editor destroys it.** `InstrumentEditor::close` (bare `v` on a shown
  editor, the window's title-bar button, or the plugin closing its own floating
  window) runs the same `teardown_gui` as a per-track plugin remove: GUI
  extension `destroy` plus dropping the parent `NSWindow`, leaving `created =
  false` so the next `show` rebuilds from scratch. This is what mainstream hosts
  do, and it is the point of the whole thing: a merely hidden editor keeps the
  plugin's *own* repaint/parameter-poll timers running (JUCE `Timer`s and the
  like, which the host cannot stop), so several hidden editors cost measurable
  idle CPU for UIs nobody is looking at. Only the **view** dies — parameters,
  preset and the audio processor are untouched, so the instrument keeps playing
  and `save_state` is unaffected. The cost is rebuild latency on reopen (tens of
  ms; a few hundred for a heavy UI) and the loss of view-local state such as
  scroll position or which editor tab was open. `teardown_gui` stashes
  `PluginWindow::frame_top_left` in `ClapEditor::window_top_left` and
  `create_embedded` hands it back to `PluginWindow::new`, so a reopened editor
  lands where the user left it instead of re-centering; `PluginWindow::new`
  centers only when there is no remembered position (the first open).
  Each frame `pump(yield_to_main)` also calls
  `PluginWindow::set_level(editor_level(visible, yield_to_main))` (`window.rs`,
  `EditorLevel`): a shown editor window `Float`s at `NSFloatingWindowLevel` —
  above the main window, so adjusting a track fader / transport doesn't send it
  behind — but is `Normal` (`NSNormalWindowLevel`) when Stev is not the
  frontmost app, so it never hovers over unrelated windows, and `Behind` while a
  project dialog is open on the main window (Save As, the unsaved-changes
  prompt — `060`), so the dialog isn't hidden behind it: `Display` passes
  `yield_to_main` = a dialog is open to every editor's pump. Lowering alone
  isn't enough — a window dropped to `NSNormalWindowLevel` keeps its place on
  top of the normal windows (found in testing 2026-10-05) — so `Behind` also
  `orderBack`s a shown window, and the main window is raised and given the
  keyboard the frame *after* the dialog opens, once the pump has lowered the
  editors (`sync_project_dialog_window`; this matters for ⌘Q pressed in an
  editor). `set_level` only touches AppKit on a change (`Cell<EditorLevel>`
  guard). `editor_level` reads
  `NSApplication::isActive` — a per-frame poll, deliberately not an
  activation-notification observer (the codebase avoids AppKit observers — see
  the exit-shutdown notes).

## Module layout

### Format-agnostic core

| File | Responsibility |
|---|---|
| `mod.rs` | `start_plugin_host` (build `InstrumentMixer`, `EngineHandle::add_source` it into the audio engine, spawn the `"plugin-host"` reclaim thread), `start_plugin_catalog_scan` (spawns the `"plugin-catalog-scan"` thread), `load_instrument` (**the one place a format is chosen** - dispatches on `entry.format`, then `Insert`s the returned voice), `PluginAudioHandle` (`cmd_tx` / `voice_dropped_rx` / `shutdown` / `sample_rate` / `repaint` - the egui context slot `start_plugin_host` is handed once, so `load_instrument` takes no format-specific arguments), `LoadedInstrument`, `reclaim_voices` (the `"plugin-host"` thread loop). Voices are built for `core::audio::MAX_FRAMES`. |
| `voice.rs` | The `InstrumentVoice` trait and `VoiceMix` - the mixer-owned `sleeping` / `rendered` flags and gain-ramp endpoints every voice carries. Also `note_reset_messages`, the burst `load_instrument` queues into every new voice. They live on the voice rather than in a parallel array in the mixer because the render pass is the loop spread across worker threads, where each worker may only touch its own voice. |
| `editor.rs` | The `InstrumentEditor` trait - the `!Send` main-thread half. `close` (= `teardown_gui`) and `toggle` (on `is_open`) are default methods, so a format supplies only `is_open`. |
| `buffers.rs` | `AudioIoLayout` (a plugin's declared channels per input/output port or bus; `new` applies the one-stereo-output fallback) and `PortBuffers` - the `[port][channel]` buffer set every voice renders into: silent input feed, capacity-reserved outputs, `prepare_outputs(frames)`, `main_sample` (port-0-only, mono/empty/out-of-range-safe - what the mixer sums) and `main_output_silent`. Each format builds its own FFI view over it. Unit-tested. |
| `mixer.rs` | `InstrumentMixer` (`impl AudioSource` - the audio-callback state: `[Option<Box<dyn InstrumentVoice>>; MAX_TRACKS]`, the `pending` scheduling buffer and per-block dispatch, the render/sum two-pass split, the per-track gain ramp, summed into the engine's `mix`), `PluginHostCommand`, `within_block_offset`. `AudioClock` lives in `core::audio`. Unit-tested: the pending sort's note-off/note-on ordering, `within_block_offset` clamping, the partition-point sub-block selection. |
| `transport.rs` | `TransportState` (the shared atomics) and `BlockTransport` (the plain `Copy` snapshot taken once per block and handed to every voice, in app-native units - ticks and microseconds-per-quarter - plus `bpm()`, `bar_number()` and `bar_start_beats()` - the bar maths in the project's `Meter`, carried in the snapshot from the `SharedAtomics.meter` atomic, floored for a pre-roll playhead; `bar_start_beats` is in quarter notes). Each format converts it in its own `render_block`; that conversion is a handful of float ops, so doing it per voice rather than once per block costs nothing and keeps every plugin type out of the mixer. Unit-tested. |
| `catalog.rs` | `PluginFormat` (label - which is also its `Audio/Plug-Ins` subdirectory - and bundle extension), `ALL_FORMATS`, `PluginCatalogEntry { format, bundle_path, plugin_id, name }`, `installed_bundles` (the shared depth-capped bundle walk over the install roots - what each format's scan calls), `scan_catalog` (runs every format's scan and reports the merged, name-sorted list **once per format** — VST3 is slow enough that waiting for it would hide the CLAP plugins too; `merge_scans` takes the scans as functions so each only starts after the previous one's delivery), `available_bundles_hint`. Runs on the background `"plugin-catalog-scan"` thread, result held in `Display::plugin_catalog`. Unit-tested. |
| `shutdown.rs` | `HostShutdown` - app-exit coordination (`requested` + `in_process`), one instance shared by the mixer and the reclaim thread. Unit-tested. |
| `window.rs` | `PluginWindow` - a minimal native `NSWindow` parent for an embedded plugin GUI; `show` (`orderFront:`), `is_visible`, `frame_top_left` / a `top_left` argument to `new` (position carried across a close/reopen), `set_content_size` (anchored at the top-left corner); registers/deregisters itself with `key_guard` on construction/`Drop`. It is closed by `Drop`, not hidden - there is no `hide`. Format-agnostic. |
| `key_guard.rs` | App-wide `NSEvent` local monitor reserving bare `Space`/`v`/`.`/`0` even while an embedded editor window has OS keyboard focus - `install_key_guard`, `take_toggle_editor_pending`, `register_window` / `unregister_window`. See the module doc and the caveat below. Format-agnostic. |

### `clap/` - the CLAP format

See also `180-vst3-host.md` for `vst3/`, the second format module.

| File | Responsibility |
|---|---|
| `mod.rs` | `scan_catalog()` and `load(handle, track, entry, state) -> LoadedInstrument` - CLAP's two entry points, the only items the core calls. The egui repaint slot the host needs comes from `handle.repaint`. |
| `discovery.rs` | `scan_catalog()` -> `Vec<PluginCatalogEntry>` - walks the installed `.clap` bundles (via the shared `catalog::installed_bundles`), loads each bundle's factory, enumerates descriptors. Also owns `ENTRY_CACHE` / `cached_entry` - the process-wide `PluginEntry` cache shared with `engine::load` (see below). |
| `engine.rs` | `load(bundle_path, plugin_id, sample_rate, max_frames, repaint, state)` - get the bundle's entry via `discovery::cached_entry`, pick the descriptor (display name via `discovery::descriptor_name`, shared with the catalog), instantiate, apply the persisted CLAP `state` blob, query the audio-port layout (`query_audio_io` -> `AudioIoLayout`, before activation), activate. Returns `LoadedPlugin { instance, processor, io, name }`. No hardcoded plugin consts. |
| `host.rs` | `StevClapHost` (`HostHandlers`) + `ClapHostShared` / `ClapHostMainThread`; log + gui + timer + state + thread-check host extensions (thread-check: main = AppKit main thread, audio = threads marked by `core::audio::enter_render_thread` — the `cpal` callback and every `WorkerPool` worker); `Timers` helper (unit-tested). |
| `editor.rs` | `ClapEditor` - owns one `!Send` `PluginInstance` and `impl InstrumentEditor`; `create` (lazy, on first show), `show` / `is_open`, per-frame `pump`, `save_state` (CLAP `state.save` for persistence), `teardown_gui` (GUI `destroy` + window drop + remember its position - used by both `close` and a per-track remove), `deactivate` (after the voice-dropped ack). |
| `voice.rs` | `ClapVoice` - `impl InstrumentVoice`; per-plugin audio state, `new` takes the plugin's `AudioIoLayout` and builds a `PortBuffers` to match (sized with `buffers::channel_total`). Also the private `midi_bytes_to_clap_event` (takes a sample offset) and `transport_event` (`BlockTransport` -> CLAP `TransportEvent`). Unit-tested: the event translation (incl. the note-reset burst), the transport translation. |

## Channels

Every ring the mixer touches on the realtime `cpal` audio callback thread is an
`rtrb` SPSC ring, not a `crossbeam_channel` — a channel's internal segment
allocate/free isn't safe there. That covers the command feed (`cmd_rx`), both
MIDI feeds (`clip_instrument_midi`, `live_instrument_midi`, drained), and the
removed-voice hand-off (`dead_tx`, pushed — see the `PluginAudioHandle` table
below). `rtrb` is *strict* single-producer: a `Producer`/`Consumer` half isn't
`Clone`, unlike a `crossbeam_channel` `Sender`/`Receiver`, so a feed with two
logical producers (clip + live) can't share one ring — it needs two.

Defined in `shared_channels.rs` / created in `setup.rs` (cross-platform types;
consumer dropped off macOS — a push there fills the ring once, then fails
from then on):

| Channel | Direction | Payload |
|---|---|---|
| `clip_instrument_midi_tx/rx` | `Sequencer` → mixer | `ClipInstrumentEvent { track, message: Midi3, when }` — `when` is `At(Instant)` for a scheduled tick, `Immediate` for a seek / stop note-off |
| `live_instrument_midi_tx/rx` | `MidiInputForwarder` → mixer | `(track_idx, Midi3)` live keyboard, tagged by `SharedAtomics.live_instrument_target`. No timestamp — played at the head of the callback, deliberately un-delayed |

`MidiInputForwarder` wraps its `Producer` in `Arc<Mutex<_>>` — not for the
audio thread's sake (the lock is only ever taken on the MIDI-input thread),
but because `forward_messages_to_named` can run again for a port reconnect
and each call needs to move a fresh handle into a new callback closure, which
a bare non-`Clone` `Producer` can't provide.

Created in `setup_shared_atomics` / `setup.rs`, cross-platform (mirrors
`arm_channel`'s "always exists, only meaningful on macOS" shape):

| Atomic | Written by | Read by |
|---|---|---|
| `live_instrument_target: Arc<AtomicI32>` | `Display::set_live_instrument_target` (macOS) | `MidiInputForwarder`'s input callback, to tag live-thru sends |
| `track_mix: Arc<TrackMixAtomics>` (`[AtomicU32; MAX_TRACKS]` × 2 for volume/pan `f32` bits, `[AtomicBool; MAX_TRACKS]` × 2 for mute/solo) | `Sequencer::set_track_volume` / `set_track_pan` / `toggle_track_mute` / `toggle_track_solo` (sequencer thread), reset by `new_project` | volume/pan: `InstrumentMixer::render_into` once per block (gain per voice) + `Display::draw_arranger_view` once per frame. mute/solo: `Sequencer::tick` / `chase_notes` (gate note emission, both output routes) + `Display` (draw the S/M buttons). All-platform |

Created in `main` inside the macOS block, carried by `PluginAudioHandle`:

| Channel | Direction | Payload |
|---|---|---|
| `cmd_tx` → `cmd_rx` | `Display` → mixer | `PluginHostCommand` (Insert / Remove) — an `rtrb` ring (same realtime-consumer reasoning as the MIDI feeds above) |
| `dead_tx` → `dead_rx` | mixer → `"plugin-host"` reclaim thread | `(track, Box<dyn InstrumentVoice>)` — dropped off the audio thread. An `rtrb` ring (`DEAD_VOICE_RING_CAPACITY` = 16): the mixer pushes from the audio callback, so it can't be a `crossbeam_channel` whose `send` may allocate. Traffic is only user-driven Insert/Remove, never per-block |
| `voice_dropped_tx` → `voice_dropped_rx` | `"plugin-host"` reclaim thread → `Display` | `track` — ack; `Display` then deactivates + drops the instance |
| `add_source` ring | `start_plugin_host` → audio engine | `Box<dyn AudioSource>` (the `InstrumentMixer`) — handed to the running engine once, an `rtrb` ring in `core::audio::engine` |

Created in `main` by `start_plugin_catalog_scan`, attached with `Display::attach_plugin_catalog_rx`:

| Channel | Direction | Payload |
|---|---|---|
| `plugin_catalog_rx` | `"plugin-catalog-scan"` thread → `Display` | `Vec<PluginCatalogEntry>` — sent once; `pump_instrument_editors` drains it and drops the receiver |

### Process-wide entry cache

`discovery::ENTRY_CACHE` (a `Mutex<HashMap<PathBuf, PluginEntry>>`, not a
channel) caches every bundle `PluginEntry` this process has loaded, keyed by
bundle path. `PluginEntry` is `Send + Sync + Clone` — only `instantiate()`'s
resulting `PluginInstance` is `!Send` — so both `scan_catalog()` (background
thread) and `engine::load()` (main thread, per plugin pick / per instrument
track on project load) go through `discovery::cached_entry(bundle_path)`
instead of loading the dylib directly. A bundle's dylib-load + CLAP `init()`
therefore happens **at most once per app run**: after the catalog scan has
reached a bundle, picking one of its plugins (or restoring several tracks
that share it) skips straight to `instantiate()`/`activate()`. Never evicted
for the process lifetime — consistent with a plugin installed while the app
runs not being picked up until restart (already true of the catalog itself).

## Sample-accurate scheduling

The path from a sequencer tick to a plugin event carries a monotonic
`std::time::Instant` end to end, so a clip note lands at the output sample its
musical position calls for instead of being quantised to the audio-block
boundary.

1. **`Clock`** (`core/clock.rs`) credits musical time from the *real* elapsed
   nanoseconds between timer firings (`Clock::musical_credit_ppqn_us`), not a
   nominal 1 ms — this also removed a ~1.6 % tempo error on Apple Silicon, where
   the mach timer's period was stretched to ~1.016 ms. Each tick a firing
   produces gets an intended `Instant`, linearly interpolated across the firing's
   elapsed span (`Clock::tick_offset_ns`), and rides `tick_tx` as
   `ClockTick { at, tick }`. A firing's elapsed time is clamped
   (`MAX_ELAPSED_NS`, 100 ms) so a debugger pause can't dump a huge tick burst.
2. **`Sequencer::tick(at)`** stamps each `Instrument` track's clip events with
   `EventTime::At(at)` onto the `rtrb` clip ring.
3. **The audio engine** (`core/audio/engine.rs`) calls `AudioClock::observe`
   once per callback — `Instant::now()` at entry mapped onto the `steady` sample
   counter, anchored on the first callback then nudged by `ALPHA` (0.05) of the
   observed error (hard re-anchor past `RESYNC_SECS`); this absorbs callback
   jitter and tracks device-vs-system clock drift without accumulating error.
   The engine then calls each source's `render_into(mix, frames, first_frame,
   &clock, now)` per sub-block (`frames` ≤ engine `MAX_FRAMES`; in practice one).
4. **`InstrumentMixer::render_into`** (`core/plugin_host/mixer.rs`):
   - Each clip event's `At(at)` becomes `RenderCtx::scheduled_frame(at)` =
     `frame_for(at) + SCHEDULE_DELAY_FRAMES`, never before the block start;
     every live event becomes `first_frame`; an `Immediate` event becomes
     `first_frame` *or the latest `target_frame` its track already has in
     `pending`*, whichever is later (`immediate_target_frame`) — so a stop / seek
     note-off can never overtake a clip note-on sent just before it that is
     still held one buffer ahead (it would strand that note-on). The delay (≈one
     buffer) is what lets an event that arrived slightly late still get a
     positive in-block offset. Events go into `pending` (pre-reserved
     `PENDING_CAPACITY`), sorted `(target_frame, seq)` — the `seq` tie-break
     stops a non-stable sort from putting a note-off before its note-on at one
     frame. Events past this block's end stay in `pending` for a later call.
   - `pending`'s sorted prefix of events due before `first_frame + frames` is
     drained into voices at `within_block_offset`, a `TransportEvent` is built
     (from `TransportState` atomics), the voices render one CLAP block at
     `first_frame`, their output is summed into `mix` **through the per-track
     volume/pan gain, forced to `0.0` for a muted / non-soloed track**
     (`target_gains()` from `track_mix`, ramped across the block — see the
     "Per-track volume / pan" and "Per-track mute / solo" bullets above), then
     `in_events` is cleared.

## Per-track teardown ordering

`clack` leaks a `PluginInstance` (safely) if it drops while its
`StoppedPluginAudioProcessor` still holds an `Arc` clone. So removing a track's
plugin is a handshake:

1. `Display::remove_track_instrument` (by position) / `remove_slot_instrument` —
   `InstrumentEditor::teardown_gui` (destroy GUI), send
   `PluginHostCommand::Remove { track: slot }`, stash the editor in
   `pending_instance_drop` under the slot.
2. Mixer takes the voice out of its slot → `dead_tx`.
3. The `"plugin-host"` reclaim thread (`reclaim_voices`) drops the `Box<dyn InstrumentVoice>`
   (drops the processor, off the audio thread) → `voice_dropped_tx.send(track)`.
4. `Display::pump_instrument_editors` drains the ack → `InstrumentEditor::deactivate`
   (`instance.try_deactivate`, now that the processor Arc is gone) → the editor
   drops, cleanly destroying the instance.

Between steps 1 and 4 the track is silent (mixer slot empty) but the instance is
still alive and idle — safe.

## Track add / remove

Adding or removing a track (`050-undo-redo.md` § Track add / remove) never
moves a voice: engine state is keyed by slot (above), a removed track frees
its slot, an added one takes the lowest free one.

- **Remove** (`Delete` with the track headers focused, the output menu's `Delete track`, or the undo of an add).
  The sequencer lifts the track and first releases its sounding plugin notes on
  its slot (clearing that slot's `InstrumentNotes`). The handler sends
  `UiEvent::TrackInstrumentRemoved { slot, track_id }` for every removed
  track; `Display::park_track_instrument` does nothing if the slot holds no
  plugin, else saves the live plugin's state (`InstrumentEditor::save_state`)
  into `InstrumentHost::parked_states` under the track id and tears the plugin
  down by the ordinary handshake above. No voice is parked anywhere, and the
  parked state never leaves `Display` (the sequencer's `InstrumentRef.state`
  stays as of the last ⌘S, which re-captures every loaded plugin anyway).
- **Restore** (the undo of a remove, the redo of an add). `restore_track` puts
  the track back in its own slot (or the lowest free one); the handler sends
  `UiEvent::TrackInstrumentRestored { slot, track_id, instrument }`, and
  `Display::restore_slot_instrument` reloads the plugin into that slot with
  the editor closed — from the parked live state if there is one, else from
  `instrument`'s blob — the project-load path (`load_slot_instrument` with
  `picked: None`). So undo brings the plugin back as it was when the track
  left, not as of the last save. Keyed by slot, it doesn't depend on
  `Display`'s track mirror being up to date. A plugin that is no longer
  installed is logged and the track stays an italic, not-loaded plugin track.
- **A project load** puts every track back in slot = position
  (`Sequencer::new_project`), and `sync_instruments_to_tracks` drops every
  parked state and reloads every plugin into its slot (`TrackInstrumentsChanged`'s
  specs are `(slot, InstrumentRef)`).

## App-exit shutdown (`shutdown::HostShutdown`)

On macOS `eframe::run_native` never returns — AppKit's `terminate:` calls
`exit()` right after `App::on_exit`, and `on_exit` itself runs **nested inside**
AppKit's own `-[NSApplication terminate:]` notification post (winit's
`app_will_terminate` is an observer callback for that same post, on the same
thread). `Display::on_exit`:

1. `handle.shutdown.request_and_wait()` — sets `requested`, unparks the
   `"plugin-host"` reclaim thread, blocks (500 ms cap) until the mixer is not
   inside a `process()` call (`in_process`). Once `requested` is set,
   `InstrumentMixer::render_into` returns at its first line without touching any
   plugin, so after this returns no `process()` can run. This part is required —
   without it the audio callback can call into a plugin the process is about to
   unload. The still-loaded `ClapVoice`s stay in the mixer on the parked engine
   thread and **leak** — dropping them would re-enter the plugin code `exit()`
   is unloading; `exit()` reclaims the memory anyway. (Only voices already
   handed to `dead_rx` by a just-issued Remove get dropped, by the reclaim
   thread's final drain.)
2. Every `ClapEditor` (`track_instruments` and `pending_instance_drop`) gets
   `teardown_gui()` (GUI extension `destroy` + close the host `NSWindow`),
   then is **leaked** (`std::mem::forget`) rather than fully dropped:
   - `teardown_gui()` runs — it's the same call ordinary per-track teardown
     already makes constantly, so it's known-safe, and it's required here:
     leaving an editor's `NSWindow` un-closed at exit left a stale entry in
     AppKit's window list that crashed on a later event
     (`-[NSApplication _indexOfWindow:]` reading a corrupted weak reference
     while routing the next event — no Stev frames in that crash's stack,
     but it lined up with a window left open at quit).
   - The plugin *instance* is still never dropped — explicitly calling CLAP's
     full `destroy()` from this reentrant context crashed for a JUCE-based
     plugin (Surge XT): its `destroy()` tears down JUCE's `Desktop` singleton,
     which calls `-[NSDistributedNotificationCenter removeObserver:...]`,
     reentering CoreFoundation's notification-registrar machinery while it is
     already mid-iteration on the same thread (`CFEqual` segfault on a freed
     page). `clack` already tolerates a leaked, never-destroyed
     `PluginInstance` (see the per-track teardown-ordering note above), and
     `exit()` follows immediately, so there is nothing gained by destroying it
     here.
3. The process ends with **`exit_without_destructors(0)`** (`libc::_exit`)
   after flushing stdout/stderr,
   never returning to AppKit's `exit()` (nor, on a window close, to `main`'s
   `process::exit`, now only a fallback). `exit()` runs every loaded module's
   static destructors, and with the instances leaked those tear down plugin
   runtimes still in use: Native Instruments' Qt-based plugins (Kontakt 8,
   FM8) segfaulted in `QApplication::~QApplication` → `qAccessibleCleanup` on
   every quit. `_exit` skips static destructors and `atexit` handlers
   altogether; the OS reclaims everything the process owns. Anything that must
   reach disk at quit has to be written before this point.

## Main-thread GL context (`view::appkit::MainGlContext`)

Stev's window is drawn with OpenGL (eframe's glow backend), on the main
thread — the same thread every plugin editor runs on. A plugin that renders
its own UI with OpenGL makes *its* context current there and may leave it
current: Native Instruments' Qt Quick editors (Kontakt 8, FM8) do, from
inside our editor pump, while their view is being opened, and on mouse-overs
between frames.

eframe does not recover from that. Before each paint it makes its context
current only if glutin's `Surface::is_current` says it isn't, and on macOS
that only checks that the context is attached to our view, which is always
true, never which context is current on the thread. Egui then paints into
the plugin's context (`GL_INVALID_FRAMEBUFFER_OPERATION`), and the main
window turns black — even with the editor closed — while the app keeps running
underneath — input is handled, ⌘Q's unsaved-changes prompt opens, none of
it visible. The plugin's own window looks fine.

So `main` captures eframe's `NSOpenGLContext` in the app-creation closure
(where it is current), and the **last thing `Display::ui` does** is make it
current again if anything else is. Last, because `ui` itself can call into a
plugin (opening an editor), and nothing between the end of `ui` and eframe's
paint runs plugin code. One `+currentContext` query per frame when nothing
changed.

The root cause is upstream, in glutin's CGL backend (`api/cgl/surface.rs`,
`Surface::is_current`). Once that compares against `+currentContext`, this
restore can go. eframe's wgpu (Metal) renderer has no current-context state
at all and would sidestep the whole class of clash — a much bigger change,
worth weighing only if other GL-drawing plugins turn up new ones.

## Project persistence

`TrackOutputData::Instrument { bundle_path, plugin_id, display_name, state }`
(all `#[serde(default)]`; `state` also `skip_serializing_if` empty).
`apply_to_sequencer` maps it back to `TrackOutput::Instrument(InstrumentRef)`; an
empty `bundle_path` (a phase-3 project, or one saved before a plugin was picked)
falls back to `MidiOut { 0 }`.

On project load / new-project the sequencer thread emits
`UiEvent::TrackInstrumentsChanged { specs }`; `Display::sync_instruments_to_tracks`
tears down every current editor and loads an **editor-less** plugin for each
`Instrument` track named in `specs`. A plugin the project wants but that is not
installed logs a warning and leaves the track silent.

### Plugin state / presets

`InstrumentRef.state` is the plugin's CLAP `state` blob (active preset + knob
positions), persisted base64-encoded in the `.stev` and re-applied on load:

- **Save.** The ⌘S handler calls `Display::capture_instrument_states` *before*
  sending `ConfirmFilename`: for each loaded editor, `ClapEditor::save_state`
  (CLAP `state.save`) → `InputEvent::CaptureTrackInstrumentState { slot, state }`
  → `SequencerCommand::SetTrackInstrumentState` → `Sequencer::set_slot_instrument_state`
  writes it onto the `InstrumentRef` of the track in that slot. All four events queue ahead of
  `ConfirmFilename` → `SaveProject` on the same channel, so `from_sequencer`
  serializes the fresh blob. A plugin with no `state` extension, or a failed
  save, is skipped (the last good blob is kept).
- **Load.** `sync_instruments_to_tracks` passes `InstrumentRef.state` into
  `load_instrument` → `engine::load` calls CLAP `state.load` **after instantiate,
  before activate** (a fresh instance is expected to receive its preset there).
  A corrupt / partial blob is logged and ignored — the plugin stays at its
  default, project load never fails.
- `StevClapHost` declares the `state` host extension; its `mark_dirty` is a
  log-only no-op (state is captured fresh on every save, not tracked).
- The restore path passes `picked: None` to `load_slot_instrument` so it
  does **not** re-emit `SetTrackOutput` — the sequencer already holds
  the right `TrackOutput` (with its state) from `apply_to_sequencer`, and
  re-announcing would clobber it.
- Presets are captured only on ⌘S. Changes made in an editor after the last
  save are not persisted on quit, and don't trigger the unsaved-changes
  prompt on their own: many plugins write a different state blob for the
  same preset every time, so the check leaves state out (`060` § Unsaved
  changes).
- **Note-reset on instantiation.** Restored state can carry held-note state
  (u-he Repro's on-screen keyboard came back showing stuck keys over CLAP —
  cosmetic, no audio). `load_instrument` therefore queues All Sound Off + All
  Notes Off + Reset All Controllers + an explicit Note Off for every key into
  every new voice, whatever its format, for its first processed block
  (`voice::note_reset_messages`), so a restored plugin always starts silent.
  Queued on the main thread before `Insert`, so it is off the audio path. VST3
  only sees the three CCs if its `IMidiMapping` maps them; the per-key Note
  Offs (id −1, VST3's "no id") always arrive, and the 256-event list keeps
  room for the block's own events.

## Known caveats

- **Editor float level follows app activation with a one-frame lag.**
  `set_level` is driven from the per-frame `pump`, so when you switch back to
  Stev the main window can briefly draw over the editor before the next
  frame raises it again. eframe repaints on focus changes so it is normally
  imperceptible; if the app is fully idle (visible editor, plugin requesting no
  timer) the drop-to-normal on switching *away* can also lag a frame. Not worth
  a continuous repaint or an activation observer to close.
- **Bare `Space`/`v`/`.`/`0` are reserved even while an embedded editor has OS
  keyboard focus** (`key_guard.rs`) — an app-wide `NSEvent` local monitor
  intercepts them before AppKit routes them to whichever window is key, but
  only while the focused window is one of our own tracked embedded editor
  windows, never the main app window, so in-focus behavior is untouched.
  `.`/`0` mirror the in-focus bindings' classic DAW numpad transport pair
  (stop / play from cursor); the monitor deliberately does **not** treat
  `NumericPad` as a blocking modifier, or the guard would silently never fire
  from the numpad the pairing is named for. The unavoidable tradeoff: while an
  editor window is focused, none of these four can be typed as a literal
  character into anything inside the plugin's own UI (a preset-name field,
  say) — every other key, and any modified chord (⌘V, Shift+V, …), still
  reaches the plugin normally. Does **not** apply to a floating plugin GUI
  (rare — most plugins don't support it): it owns its own native window,
  invisible to this registry.
- **Clip event latency is a constant one buffer** (`SCHEDULE_DELAY_FRAMES` in
  `core/audio/mod.rs` = 256 frames ≈ 5.3 ms at 48 kHz, matching the engine's requested
  `DESIRED_BUFFER_FRAMES`). Clip
  events are placed at their exact output sample within that budget; the delay
  is what buys the mixer room to do so even for an event that arrived up to a
  buffer late. It is a **floor** — an event that arrives later still degrades to
  block offset 0, no worse than the pre-scheduling behavior. **Live keyboard
  events skip the delay entirely** (offset 0, lowest playing latency), so clip
  and live can sit up to ~5.3 ms apart from each other — an accepted tradeoff.
- **Song position is tick-granular.** The `TransportEvent`'s `song_pos_beats` is
  derived from `playback_tick`, which only advances once per sequencer tick, so
  a plugin polling the playhead sees it step, not glide. `tempo` is exact.
- **Only the main output port feeds the mix.** `engine::load` queries the
  plugin's full audio-port layout (`audio-ports` extension → `AudioIoLayout`)
  and `ClapVoice::new` allocates a buffer set that matches it — every input
  port and every output port. A plugin that declares more than one stereo pair
  (an Access Virus emulation, say, with three output buses and an analog-in
  bus) is still rendered in full, but the mixer's summing pass reads only
  output port 0 through `ClapVoice::sample` — the aux ports are written by
  `process()` and discarded. A mono main port feeds both mix channels; a
  plugin with no `audio-ports` extension is given one stereo output. This is a
  *crash* fix, not a nicety: `process()` with a port the plugin declared but
  the host never supplied dereferences a null channel pointer inside the
  plugin (seen zeroing 256-frame aux-output channels in OsTIrus). It bit on
  the first `process()` call, so a project restoring such a plugin crashed on
  load, no editor involved.
- **The input feed is silent.** `in_bufs` is filled with zeros once at
  `ClapVoice::new` and never written again (channels flagged `is_constant`);
  it exists only so a plugin with an audio input bus is handed real buffers.
  Stev hosts instruments, not audio-FX — nothing routes audio *into* a
  plugin.
- **`request_callback` / `register_timer` take a short egui lock** —
  `request_callback` can be invoked from the audio thread (per its own doc
  comment in `host.rs`), so it's the same class of not-strictly-realtime-safe
  shortcut the MIDI/command rings were fixed to avoid, just not yet addressed.
- **Embedded editor window forwards a plugin-initiated resize, but not the
  reverse.** `HostGuiImpl::request_resize` (`host.rs`) — fired when the plugin
  asks its host parent to change size, e.g. zooming the editor — stashes the
  size (`resize_pending`/`resize_width`/`resize_height`, same
  atomics-plus-`wake_ui`-plus-drain-in-`pump` pattern as `gui_closed`) for
  `ClapEditor::pump` to apply via `PluginWindow::set_content_size`, so the
  window now grows/shrinks to match. Floating GUIs are unaffected either way
  (the plugin owns its own OS window and resizes it directly; `pump` discards
  the request when `self.window` is `None`). The other direction is still
  missing: `PluginWindow` has no `Resizable` style mask and no delegate, so
  the user cannot drag-resize the window and have that forwarded back to the
  plugin via `gui.set_size` / `gui.adjust_size`.
- **Plugin state is captured only on ⌘S** — a preset changed in an editor after
  the last save is not persisted on quit (no autosave hook), and the
  unsaved-changes prompt doesn't catch it (`060` § Unsaved changes: state
  blobs aren't stable enough to compare; the plugins' own change
  notifications would be the way). The `params`
  extension is still not implemented; `state` alone covers the common synths.
- **Stuck-note safety** — `Sequencer::release_instrument_notes` (transport stop /
  seek) sends an explicit Note Off (channel 0) for exactly the clip-driven notes
  it recorded as sounding — no blanket All Notes Off. The explicit note-offs are
  what clear a Repro-style on-screen keyboard, which ignores CC 123 for its
  display; release tails still ring. Held notes are tracked per track in
  `sequencer/instrument_notes.rs` (`InstrumentNotes`, unit-tested), fed from
  `Sequencer::tick` / `chase_notes` (clip route) and `handle_midi_input_dispatch`
  (live route), since the plugin MIDI path never passes through the `"midiout"`
  thread's `NoteLogger`. This mirrors the `NoteLogger` rule: a note the player is
  **holding live on the armed instrument track is never released** by a stop, so
  start/stop mid-improvisation doesn't cut it — it stays recorded and a later
  stop (after key-up) catches it. Removing, shrinking, or shifting a clip
  while it is playing is a separate path with no transport-stop equivalent to
  fall back on: `DeleteInRangeEdit`, `SplitClipsEdit`,
  `InsertSilenceEdit` and `PasteClipsEdit::undo` all
  call `Track::release_sounding_notes_for_clip` before mutating the clip,
  queuing its open notes' offs through `Track::pending_note_offs` so `tick()`
  still emits them (the clip is gone, shrunk, or moved before it otherwise
  could, and the CLAP route has no `NoteLogger`-equivalent safety net of its
  own). The note would otherwise ring indefinitely — unlike the live-hold case
  above, there is no later transport stop that would catch it. Notes a plugin generates entirely on its own
  (a free-running arpeggiator not tied to a held note) are not covered; releasing
  the source note stops a normal arp.
- **Loop wrap must not leak the next clip's notes.** A loop wrap re-anchors the
  sequencer *synchronously* from the `"sequencer"` tick pump —
  `Transport::tick` → `TickOutcome::Wrapped` → `EventHandlers::reanchor_playback`
  — before the next `Sequencer::tick`. When the re-anchor was deferred through
  `TransportEvent::PlaybackTickReset` instead, a burst of ticks drained in one
  `select!` wakeup ran `Sequencer::tick` past the region end, so the track
  seeked into the following clip and fired its opening note-on into the plugin.
  `release_instrument_notes` then sent a matching `EventTime::Immediate` note-off,
  but that is dispatched a block *ahead* of the leaked `EventTime::At` note-on
  (which carries the one-buffer `SCHEDULE_DELAY_FRAMES`), so it missed — leaving
  one unmatched note-on per affected wrap. See `010-keybindings.md` § Loop State and
  `150-clock-position-sync.md`. The same overtaking hit any stop, seek or wrap
  landing within one buffer of a clip note-on (rare, timing-dependent stuck
  notes); the mixer now places every `Immediate` event after its track's queued
  events (`immediate_target_frame`), which closes the whole class.
- **The catalog is scanned once**, on the background `"plugin-catalog-scan"`
  thread started right after `start_plugin_host`, and cached for the session
  (`Display::plugin_catalog`) — a plugin installed while the app runs won't
  appear until restart. While the scan runs the browser's Plugins
  category ends in a "Scanning…" row rather than reading as complete.
- **`instantiate()`/`activate()` still run on the main thread, sequentially,
  once per instrument track on every project load** (`sync_instruments_to_tracks`).
  This part can't be backgrounded: the CLAP `instantiate()` call produces a
  `!Send` `PluginInstance` that has to be created on the thread that will host
  its editor, which is the main thread by this app's design. What *is* fixed:
  the dylib-load + CLAP `init()` step that used to precede every `instantiate()`
  call is now shared process-wide (`discovery::cached_entry` / `ENTRY_CACHE`,
  see Channels above), so a track's load is only expensive the first time its
  bundle is opened — by a project with several tracks sharing a bundle, or by
  a project opened after the background scan has already warmed the catalog.
  A project with several distinct, never-before-loaded bundles can still show
  a brief main-thread pause while restoring, bounded by the number of *distinct*
  bundles rather than the number of instrument tracks.

## Out of scope

Undo integration for track-output changes, undo integration for the per-track
volume/pan/mute/solo (mixer-surface controls, deliberately not undoable; solo
also not persisted — see `020-views-and-state.md`), FX inserts/sends beyond that
single gain stage,
latency compensation, a user-resizable plugin window (dragging the window edge and
forwarding that back to the plugin — see Known caveats; the plugin-initiated
direction is handled), docking editors into the main window, the `params`
extension, sub-tick playhead resolution in
the CLAP transport, a time-signature model, non-macOS.

## Verification

Build gate only, no app launch — the full list is the ground rule in
`AGENTS.md`. `cargo test` here covers (`midi_bytes_to_clap_event` +
`within_block_offset` + the `AudioClock` filter + the pending-sort note ordering,
the `Timers` helper, `note_reset_messages`, `pick_buffer_size`, `InstrumentNotes`
(held-note tracking incl. live-overlap), the `TrackOutputData::Instrument` JSON
round-trip + back-compat, the base64 plugin-state blob round-trip incl. non-UTF-8
bytes, `mix::gains_for` + the fader taper round-trip/monotonicity, the
`Sequencer::set_track_*` clamp/reset, the `TrackData` volume/pan/mute round-trip +
missing-field back-compat, `mix::track_is_audible`, the `Sequencer` mute/solo
wiring, `track_header_rects` geometry, and `Clock`'s
elapsed-time tick crediting + per-tick instant interpolation + stall clamp, and
`MachWaitUntilTimer::nanos_to_mach`).

Manual smoke test (`cargo run --release --package stev --bin stev`):

1. Track 1 selected → browser (⌘⌥B) → Plugins → a plugin → ENTER (or drag it
   onto track 1). Its editor opens; the keyboard plays it; the header's output
   chip shows its name.
2. Track 2 → a different plugin. Selecting track 2 moves the live keyboard
   to its plugin; track 1 keeps its own.
3. Arrange clips on both → transport play → each track drives its own plugin.
4. `v` closes/reopens the selected track's editor; closing the window with its
   title-bar button then `v` reopens it. Drag an editor somewhere, close it,
   reopen — it comes back in the same spot, not re-centered. Idle CPU with a few
   editors closed should sit near the no-editor baseline. With the editor open, click the main window and adjust a track
   fader / pan — the editor stays in front. Switch to another app (a browser) —
   the editor no longer floats over it. Switch back to Stev — it returns to
   the front.
5. Click an instrument track's output chip → `Remove <plugin>` (or a channel
   cell) → its plugin tears down, the chip reads `Ch N`, the footer says so.
6. Save, `⌘N`, reopen → both plugins reload with no editor, clips play, `v`
   shows them.
7. On a plugin editor, dial in a distinctive preset / knob position → `⌘S` →
   `⌘N` → reopen the project → `v`: the editor comes back with that preset, not
   the plugin default.
8. Quit — clean, no segfault.
9. Load a synth on several tracks but leave them silent for a while (no notes,
   transport stopped) — idle CPU should be lower than the same synths actively
   playing (confirms `ProcessStatus::Sleep` is taking effect, not just that the
   feature still works). Then play/arrange again on each — sound resumes
   immediately, confirming `queue_midi` wakes a sleeping voice.
10. Click into an open plugin editor window (so it visibly takes OS keyboard
    focus, e.g. click a knob) — bare Space should still start/stop the
    transport (both directions — starting again after a stop, not just
    stopping), numpad `0`/`.` should play-from-cursor/stop the same way, and
    bare `v` should still close the editor, with no click back to the main
    window needed first and no stray mouse movement needed to "unstick" it
    either. Try a modified chord too (⌘V, Shift+V) — it should reach the
    plugin normally, not get swallowed. With Caps Lock toggled on, the four
    reserved keys should still work identically. Then repeat with the main
    app window focused (editor not open, or not clicked into) — behavior
    should be identical to before this change.
11. Open the browser immediately after launch, before the background scan is
    likely to have finished (with enough installed bundles that a synchronous
    scan would be noticeably slow) — it should open instantly (no UI freeze)
    with a "Scanning…" row ending the Plugins category, then the list should
    populate on its own a moment later with no further input.
12. With several bundles installed, picking a plugin in the browser should feel
    fast (the dylib-load + `init()` cost was already paid by the background
    scan). Load the same bundle's plugin (or a second plugin from the same
    bundle) onto another track — also fast. Save a project with 2-3 instrument
    tracks on different bundles, `⌘N`, reopen it — the reload should not be
    noticeably slower than before this change.
13. On a plugin with an embedded editor and its own zoom control (u-he synths
    have one), open the editor and zoom in/out — the host window should
    resize to match the new content size (no clipped or letterboxed content,
    no stale window bounds). A floating-GUI plugin (rare), if you have one,
    should be unaffected either way — it always managed its own window size.
14. **Timing.** Arrange a dense clip (straight 16ths) on an instrument track and
    play — it should feel tight and even, with none of the block-boundary
    clustering the old offset-0 path had. Run the same pattern to an instrument
    track *and* a MIDI-Out track against external gear at once — they should
    stay aligned (constant offset is fine, jitter is not). Play the keyboard
    live on an armed instrument track — latency should feel unchanged (live
    keeps offset 0).
15. **Tempo.** Set a known BPM and check it against an external reference
    (a metronome app, or record the MIDI-Out into a DAW). It should be accurate
    now; it ran ~1.6 % slow on Apple Silicon before. Then confirm loop / seek /
    stop still release and chase plugin notes cleanly (the `Immediate` paths).
16. **Transport.** Load a plugin with a tempo-synced delay or arpeggiator — it
    should lock to the sequencer tempo and follow start/stop, not free-run.
17. **Mute / solo.** Load a synth with a long reverb / delay on 2+ tracks,
    arrange clips, play. Click **M** on one mid-note — it drops to full silence
    at once, tail included, no click, no stuck note; the other tracks are
    untouched. Click **S** on another — only it sounds; un-solo restores the
    rest. A track that is both muted and soloed stays silent. `⌘S` then reopen:
    mutes come back, solos do not. `⌘N`: both clear. Works the same on a plain
    MIDI-Out track except the outboard tail (which the app can't reach).
