# Audio Meters on Plugin Tracks

**Archived 2026-10-20. Status: built and merged 2026-10-20.** The as-built behaviour lives in `030` § Arranger Track Header and `130` § the summing pass; this file is the plan and its rationale, not maintained. Design agreed the same day. As built: the levels are published as linear amplitudes, not dB, so the atomics' all-zero default is silence; the clip colour reuses `theme::track_mute()`, red in every palette; `MeterBallistics` and `BlockDecay` live in `plugin_host/meter.rs`, macOS-only like the mixer that steps them, while `ChannelLevel` and `amplitude_db`, which the UI reads on every platform, stay in `core/audio/meter.rs`; the mixer's tests share `plugin_host/test_voice.rs`'s `ConstantVoice` with the export tap's. Found in testing: a project loaded while playing inherited the old meters' activity; a slot's meter now starts from empty on a reset (`TrackMixAtomics::meter_resets`, bumped by `reset_slot_mix`) and on a voice swap (`130`). This is the build plan for `280-roadmap.md` § Audio meters on plugin tracks. Plugin tracks meter their audio output in the track header: a stereo pair of bars, each showing peak and RMS, with a peak-hold line. MIDI Out tracks keep today's velocity meter (`030` § Arranger Track Header), drawn in the same two-bar strip. When the feature lands, the as-built behaviour moves into `030` and `130`, the 280 item comes out, and this file is archived.

## Decisions

Agreed with the user 2026-10-20:

- **Stereo.** One bar per channel, each as wide as today's MIDI meter (`TRACK_METER_W`, 4px), with a thin gap between them.
- **Neutral.** No green, no warming toward 0 dB: the meter stays in the theme's foreground tones, like the MIDI meter and the rest of the header. The coloured activity dot was rejected on 2026-10-19 as distracting, and the same reasoning applies here.
- **Peak and RMS in one bar**, as in Bitwig, Ableton and Logic. The peak bar is the taller, dimmer one; the RMS bar is the shorter, brighter one in front of it. That's what makes a DAW meter look dimmer near the top: the upper part is peak only.
- **A peak-hold line**: a thin line at the highest recent peak.

Chosen in this plan (change them here before the build if they're wrong):

- **Clipping** shows on the peak-hold line, not as a separate cap. While the held peak is above 0 dBFS, the line is drawn in a red theme token instead of the neutral one. It's the only colour the meter ever shows, and only when something is actually wrong.
- **Wider header, not narrower controls.** `TRACK_HEADER_W` grows (see § Layout), so the S/M buttons, output chip and bars keep their width and the meter keeps its gap from them.
- **Ballistics**: peak attacks instantly and falls 20 dB/s; RMS averages over ~300 ms; the hold line holds 1.5 s, then falls at the peak's rate, and jumps up at once to any higher peak. The scale is linear in dB from −60 dBFS (empty) to +6 dBFS (full), with a faint mark at 0 dBFS (changed from a 0 dBFS top after the first test).
- **Post-fader, post-pan, post-mute.** The level is each voice's output after its gain ramp, so the meter matches what the fader and pan do, and a muted or non-soloed track falls to empty (the mixer already zeroes its gain, `130`). This is what the user hears from that track; the stems in an audio export are taken before gain and aren't affected.

## Where the level comes from

`InstrumentMixer::render_into`'s summing pass (`plugin_host/mixer.rs`) already reads every gained sample of every voice to add it into the mix. Each block, for each slot and channel, it also takes:

- the block peak: the largest absolute gained sample;
- the block mean square: the sum of squared gained samples over `frames`.

A voice that didn't render (asleep, or its process call failed) and an empty slot contribute a silent block (peak 0, mean square 0), so their meters fall rather than freeze.

**The ballistics run on the audio thread, not in the UI.** The mixer runs a block every few milliseconds; the UI draws every 33 ms at best. If the mixer only published each block's peak, the UI would see one block in six or so and miss most transients. Applying the ballistics where every block is seen means nothing is missed, and the UI only reads finished values.

- **A pure `MeterBallistics`** (new, `core/audio/meter.rs`; built in `plugin_host/meter.rs`), one per slot and channel, owned by the mixer: `peak_db`, `rms` (mean square, smoothed), `hold_db` and `hold_age`. `step(block_peak, block_mean_square, dt)` advances it by one block:
  - `peak_db = max(to_db(block_peak), peak_db − PEAK_FALL_DB_PER_S · dt)`;
  - `rms += (block_mean_square − rms) · (1 − e^(−dt / RMS_TAU))`;
  - a peak at or above `hold_db` resets `hold_db` and `hold_age`; otherwise `hold_age += dt`, and past `PEAK_HOLD` the hold falls at `PEAK_FALL_DB_PER_S`, never below `peak_db`.
  - Every dB value is floored at `METER_FLOOR_DB` (−120), so silence never makes a `−inf`.
  - `dt` is `frames / sample_rate`, from `RenderCtx`.
- **Published per block** into `TrackMixAtomics::audio_level` (new, beside `note_on`), by engine slot: for each channel, peak, RMS and hold in dB as `f32` bit patterns in `AtomicU32`s, stored `Relaxed`. Like every array in `TrackMixAtomics` it's indexed by `Track::slot`. Only the audio thread writes it, and only the render thread reads it: no lock, no allocation, no channel. The three values can be read from different blocks, a few milliseconds apart, which nobody can see. An accessor `audio_level(slot) -> [ChannelLevel; 2]` returns them as `f32`s, with the out-of-range default at the floor.
- **Cost**: an `abs`, a compare and a multiply-add per sample per channel in a loop that's already there, plus one `exp` and a few compares per slot per block. Negligible next to the render pass (`170`).

When the mixer has no audio device (no engine), nothing writes the atomics, and the meters stay at the floor.

## What the header shows

- **Plugin tracks** (`TrackOutput::Instrument`): the audio meter, from `audio_level`, whether or not a plugin is loaded (with none it stays empty, which is true).
- **MIDI Out tracks**: today's velocity meter, unchanged in behaviour (`meter_level`, `TRACK_METER_FALL`), drawn as **one bar centred in the strip** (`meter_centre_rect`), so the meters still line up down the header but MIDI doesn't look stereo. A velocity is a single reading, so it's drawn in the RMS (solid) tone, with no peak bar and no hold line. (Built first as two identical bars for the same footprint; the user found a two-channel MIDI signal odd, 2026-10-20.)

### Drawing

Per channel, bottom-up in that channel's bar (`draw_track_meter`, `arranger.rs`):

1. The trough, `grid_seam_color()`, as today.
2. The peak bar, to `peak_db`, in the dimmer tone: `fg_dim` mixed halfway toward the trough colour (a blend in brightness, not alpha — `030` § Borders' rule for fills).
3. The RMS bar, to the RMS level in dB, in `fg_dim`, in front of the peak bar.
4. The hold line, 1px at `hold_db`, in `fg_dim`, or in the red clip token while `hold_db > 0`. Not drawn at the floor.

Heights map dB to the bar with a pure `db_to_meter_height(db) -> f32` (`0.0` at or below −60, `1.0` at or above +6, linear in dB between; as built, the scale first topped out at 0 dBFS, and the user found a clip had nowhere to show, so it gained Ableton's +6 dB headroom with a faint 0 dB mark, 2026-10-20). Pixel-snap each edge so the bars don't shimmer at fractional heights.

The red token: reuse an existing theme colour if one already reads as "warning red" in every palette (`track_mute()` is the candidate, to check against each palette at build time); otherwise add a `meter_clip()` token to each palette.

### Layout

Today the meter is `TRACK_METER_GAP` (4px) right of the controls and `TRACK_METER_W` (4px) wide, inside the column's 12px right padding (`TRACK_HEADER_INNER_PAD_X`), leaving 4px to the column's edge. The stereo strip is `2 × TRACK_METER_W + TRACK_METER_CHANNEL_GAP` (new, 1px) = 9px, which no longer fits with the same breathing room.

- `TRACK_HEADER_W` grows by 5px (124 → 129), and the right inset in `track_header_band` grows by the same 5px, as its own constant (`TRACK_HEADER_METER_PAD_X`, 17) instead of reusing `TRACK_HEADER_INNER_PAD_X`. The controls keep their width and position, the meter keeps its 4px gap from them and 4px to the column edge, and the whole arranger content shifts 5px right, which everything already routes through `content_origin_x()`.
- `TrackHeaderRects::meter` stays one rect, the whole strip; a pure `meter_channel_rects(meter) -> [Rect; 2]` splits it. The existing layout test (`rects.meter` width) and a new one for the split.

### Repaints

The meter rides the ~30 fps running frames as today. While stopped, `any_track_meter_falling` also counts a plugin track whose peak, RMS or hold is above −60 dB, so a live-played pad and a reverb tail after a stop keep drawing until they've fallen out of view. A live note-on still wakes the UI (`EventHandlers::request_repaint`), which starts that chain.

A plugin whose noise floor sits above −60 dB keeps the UI at 30 fps while stopped. That's correct (the meter is showing it), but it costs idle CPU. If it shows up in use, the fix is to repaint only when some bar's pixel height changed since the last frame, not to raise the floor.

## Tests

- `core/audio/meter.rs`: instant attack; the 20 dB/s fall; RMS of a full-scale sine settles at −3 dB, and of a full-scale square at 0 dB; the hold holds for `PEAK_HOLD`, then falls, never below the peak, and jumps up to a higher peak; silence floors at `METER_FLOOR_DB`, never `−inf`; `dt` from different block sizes gives the same fall over the same time.
- `plugin_host/mixer.rs`, with a constant-output test voice (as `export.rs`'s `ConstantVoice`): the meter is post-gain (a −6 dB fader reads −6 dB), post-pan (hard left leaves the right channel at the floor), and a muted slot falls; a sleeping voice falls rather than holding its last level.
- `shared_atomics.rs`: `audio_level` round-trips, and an out-of-range slot reads the floor.
- View geometry: `db_to_meter_height` endpoints, clamping and midpoint; `meter_channel_rects`; the header layout with the new widths.

## Changes elsewhere when built

- `030` § Arranger Track Header (the meter bullet: stereo, peak/RMS/hold, the clip line, MIDI Out's two identical bars, the new widths) and § Rendering Performance (the stopped repaint test).
- `130`: the summing pass's meter measurement, `MeterBallistics`, `TrackMixAtomics::audio_level`.
- `000` if its shared-atomics list names `TrackMixAtomics`' fields.
- `280`: remove § Audio meters on plugin tracks.
- `CHANGELOG.md` under `[Unreleased]`: plugin tracks now have stereo audio meters.
- `AGENTS.md`: move this file to § Archived.

## Must not regress

- The render path takes no lock and allocates nothing: the meter state is fixed-size arrays in the mixer and atomics in `TrackMixAtomics`.
- The MIDI meter on MIDI Out tracks behaves as it does today: what was sent, muted tracks empty, live notes on the selected track.
- The audio export's stems and mix are untouched: the meter only reads the gained samples the summing pass already computes.
- No coloured flash and no track colour in the meter (2026-10-19).
