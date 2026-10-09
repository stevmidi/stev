//! The audio-callback side of the plugin host: a mixer of up to
//! [`MAX_TRACKS`] per-track instrument voices, run as one [`AudioSource`] in
//! the shared audio engine (`core::audio`) — its output is summed into the
//! engine's mix, not written to a device buffer directly.
//!
//! [`InstrumentMixer::render_into`] is called once per engine sub-block on the
//! audio callback thread. Voices are added and removed at runtime via
//! [`PluginHostCommand`] (the `!Send` plugin instances live on the UI thread;
//! only the `Send` voices come here).
//!
//! Every per-track array here — the voices, the gain caches, the mix atomics
//! read in `target_gains` — and every event's `track` is indexed by engine
//! slot (`Track::slot`), which a track keeps for its whole life: adding or
//! removing a track moves nothing in the mixer. A removed track's voice
//! simply goes the usual `Remove` way.
//!
//! Nothing here is plugin-format specific: a voice is a
//! [`InstrumentVoice`], so the scheduling, transport, gain ramp and mute/solo
//! handling below are written once and serve every hosted format.
//!
//! ## Event scheduling
//!
//! Clip events arrive tagged with the [`Instant`] their sequencer tick was
//! *intended* to occur at (see `EventTime`). The engine's shared
//! [`AudioClock`](crate::core::audio::AudioClock)
//! maps that wall clock onto the `steady` sample counter, and each event is
//! held in `pending` until the block that contains its
//! [`scheduled_frame`](RenderCtx::scheduled_frame) — `frame_for(at)` plus a
//! one-buffer delay — so it lands at (close to) the right output sample,
//! with a constant one-buffer latency, instead of always at block offset 0.
//! Live keyboard events skip the delay and play at the head of the block.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};

use crate::core::audio::mix::{gains_for, track_is_audible};
use crate::core::audio::{AudioSource, INSTRUMENTS_SOURCE, RenderCtx};
use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::Midi3;
use crate::core::sequencer::{ClipInstrumentEvent, EventTime};
use crate::core::shared_atomics::TrackMixAtomics;

use super::shutdown::HostShutdown;
use super::transport::TransportState;
use super::voice::InstrumentVoice;

/// Pre-reserved capacity of the mixer's `pending` scheduling buffer. Sized well
/// over the worst realistic burst (both `rtrb` feeds — clip + live — full in one
/// callback, ~1024, plus the handful of events held one buffer ahead), so the
/// audio thread never reallocates it; an overflow only logs.
const PENDING_CAPACITY: usize = 2048;

/// Runtime control messages from the UI thread to the audio mixer.
pub(crate) enum PluginHostCommand {
    /// Slot a freshly-built voice into `track`, replacing any current one.
    Insert {
        /// Target instrument track's engine slot (`Track::slot`).
        track: usize,
        /// The built voice, of whichever plugin format the track picked.
        voice: Box<dyn InstrumentVoice>,
    },
    /// Remove `track`'s voice — the callback hands it back to the
    /// `"plugin-host"` thread to drop off the audio thread.
    Remove {
        /// Engine slot (`Track::slot`) to clear.
        track: usize,
    },
}

/// A clip/live event waiting for the sub-block that contains its target sample.
struct PendingEvent {
    /// Absolute frame in the `steady` timeline the event should sound at.
    target_frame: i64,
    /// Target instrument track.
    track: usize,
    /// The MIDI message.
    message: Midi3,
    /// Arrival order — the tie-break when several events share `target_frame`,
    /// so a non-stable sort can never put a note-off ahead of its note-on.
    seq: u64,
}

/// Where in a sub-block `[sub_start, sub_start + frames)` a pending event's
/// `target_frame` lands. An event that is overdue (target before the sub-block)
/// plays at offset 0; plugin formats require the offset to be `< frames`.
fn within_block_offset(target_frame: i64, sub_start: i64, frames: usize) -> u32 {
    (target_frame - sub_start).clamp(0, frames as i64 - 1) as u32
}

/// Where an [`EventTime::Immediate`] event for `track` lands: the head of the
/// block, unless that track already has events queued later than that — then
/// at the latest of them (and after it, by `seq`). A stop / seek note-off must
/// follow a clip note-on the sequencer sent just before it, which is still
/// held up to [`SCHEDULE_DELAY_FRAMES`](crate::core::audio::SCHEDULE_DELAY_FRAMES)
/// ahead; at the block head it would play first and strand the note-on.
fn immediate_target_frame(pending: &[PendingEvent], track: usize, block_start: i64) -> i64 {
    pending
        .iter()
        .filter(|e| e.track == track)
        .map(|e| e.target_frame)
        .fold(block_start, i64::max)
}

/// The plugin host's audio-callback state, run as one [`AudioSource`] in the
/// shared audio engine: a bank of per-track voices, the MIDI feeds, and the
/// runtime command channel.
pub(super) struct InstrumentMixer {
    /// One hosted instrument per `TrackOutput::Instrument` track, or `None`.
    voices: [Option<Box<dyn InstrumentVoice>>; MAX_TRACKS],
    /// Realtime-safe `rtrb` SPSC ring consumer (`Display` is the producer) —
    /// `crossbeam_channel`'s segment list can allocate/free, which isn't safe
    /// on this thread. See `130-plugin-host.md`.
    cmd_rx: Consumer<PluginHostCommand>,
    /// Removed voices go here for the `"plugin-host"` reclaim thread to drop
    /// off the audio thread (a plugin teardown must not run in this callback).
    /// A realtime-safe `rtrb` ring, same as the feeds above; a full ring (never
    /// reachable in practice — see `DEAD_VOICE_RING_CAPACITY`) falls back to
    /// dropping the voice here.
    dead_tx: Producer<(usize, Box<dyn InstrumentVoice>)>,
    /// [`ClipInstrumentEvent`]s from the sequencer's clip playback — a
    /// realtime-safe `rtrb` ring, same reasoning as `cmd_rx`. Each carries the
    /// intended `Instant` so the mixer can place it at the right sample.
    clip_midi_rx: Consumer<ClipInstrumentEvent>,
    /// Same, for live-keyboard events tagged by `MidiInputForwarder`. A
    /// second, independent ring: `rtrb` is strict single-producer, and the
    /// clip feed above has a different producer thread. No timestamp — live
    /// notes are scheduled at the head of the callback, deliberately un-delayed.
    live_midi_rx: Consumer<(usize, Midi3)>,
    /// Events drained from the feeds, held until the block that contains their
    /// `target_frame`. Kept sorted `(target_frame, seq)` while dispatching.
    pending: Vec<PendingEvent>,
    /// Next [`PendingEvent::seq`] value.
    next_seq: u64,
    /// Transport atomics, snapshotted once per block for the voices.
    transport: TransportState,
    /// Per-track volume / stereo balance / mute / solo, shared lock-free with
    /// the sequencer (see `TrackMixAtomics`). Read once per block by
    /// `target_gains` — mute / non-solo forces that track's output gain to
    /// `0.0` so its tail is cut, not merely its new notes.
    track_mix: Arc<TrackMixAtomics>,
    /// `(volume_db bits, pan bits)` last seen per track — so `gains_for`'s
    /// `powf` only runs when a fader actually moved, not once per block per
    /// track.
    mix_cache: [(u32, u32); MAX_TRACKS],
    /// The `(left, right)` linear gain derived from `mix_cache`, recomputed
    /// only when a fader moved.
    cached_gains: [(f32, f32); MAX_TRACKS],
    /// App-exit shutdown coordination with the `"plugin-host"` thread.
    shutdown: Arc<HostShutdown>,
}

impl InstrumentMixer {
    /// Builds the mixer with no voices, wired to the feeds and shared state.
    pub(super) fn new(
        cmd_rx: Consumer<PluginHostCommand>,
        dead_tx: Producer<(usize, Box<dyn InstrumentVoice>)>,
        clip_midi_rx: Consumer<ClipInstrumentEvent>,
        live_midi_rx: Consumer<(usize, Midi3)>,
        transport: TransportState,
        track_mix: Arc<TrackMixAtomics>,
        shutdown: Arc<HostShutdown>,
    ) -> Self {
        Self {
            voices: std::array::from_fn(|_| None),
            cmd_rx,
            dead_tx,
            clip_midi_rx,
            live_midi_rx,
            pending: Vec::with_capacity(PENDING_CAPACITY),
            next_seq: 0,
            transport,
            track_mix,
            mix_cache: [(0, 0); MAX_TRACKS],
            cached_gains: [(1.0, 1.0); MAX_TRACKS],
            shutdown,
        }
    }

    /// Resolves every track's target `(left, right)` linear gain for this
    /// block. The volume/pan part is cached — `gains_for`'s `powf` only re-runs
    /// for a track whose atomic bits changed — but the mute/solo part is a
    /// couple of `AtomicBool` loads per track, so it is applied fresh every
    /// block as a `0.0` factor. Zeroing the *output* gain (not just dropping
    /// new notes, which the sequencer already does) is what cuts a muted
    /// track's reverb / delay tail; the per-block ramp in `render_into` fades
    /// it out over one buffer, click-free. Called once per `render_into`,
    /// before the voice loop, so the borrow doesn't overlap `self.voices`.
    fn target_gains(&mut self) -> [(f32, f32); MAX_TRACKS] {
        for track in 0..MAX_TRACKS {
            let bits = (
                self.track_mix.volume_db[track].load(Ordering::Relaxed),
                self.track_mix.pan[track].load(Ordering::Relaxed),
            );
            if bits != self.mix_cache[track] {
                self.mix_cache[track] = bits;
                self.cached_gains[track] =
                    gains_for(f32::from_bits(bits.0), f32::from_bits(bits.1));
            }
        }

        let any_solo = (0..MAX_TRACKS).any(|t| self.track_mix.solo[t].load(Ordering::Relaxed));
        std::array::from_fn(|track| {
            let audible = track_is_audible(
                self.track_mix.mute[track].load(Ordering::Relaxed),
                self.track_mix.solo[track].load(Ordering::Relaxed),
                any_solo,
            );
            if audible {
                self.cached_gains[track]
            } else {
                (0.0, 0.0)
            }
        })
    }

    /// Drains runtime commands: add / remove voices.
    fn apply_commands(&mut self) {
        while let Ok(cmd) = self.cmd_rx.pop() {
            // Either way the slot's previous voice, if any, goes to the reclaim
            // thread rather than being dropped here.
            let (track, old) = match cmd {
                PluginHostCommand::Insert { track, voice } => (
                    track,
                    self.voices
                        .get_mut(track)
                        .and_then(|slot| slot.replace(voice)),
                ),
                PluginHostCommand::Remove { track } => {
                    (track, self.voices.get_mut(track).and_then(Option::take))
                }
            };
            if let Some(old) = old {
                self.dead_tx.push((track, old)).ok();
            }
        }
    }

    /// Appends an event to `pending`, assigning it the next arrival sequence.
    fn push_pending(&mut self, target_frame: i64, track: usize, message: Midi3) {
        if self.pending.len() >= self.pending.capacity() {
            dprintln!(
                "plugin host: pending-event buffer at capacity ({}); audio callback may be starved",
                self.pending.capacity()
            );
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.pending.push(PendingEvent {
            target_frame,
            track,
            message,
            seq,
        });
    }
}

impl AudioSource for InstrumentMixer {
    fn name(&self) -> &'static str {
        INSTRUMENTS_SOURCE
    }

    /// Renders one block of every loaded instrument and sums it into `mix`. The
    /// engine caps `frames` at its `MAX_FRAMES`, so this is one valid process
    /// call per voice — no internal sub-block split.
    fn render_into(&mut self, mix: &mut [Vec<f32>; 2], ctx: &RenderCtx<'_>) {
        let RenderCtx {
            frames,
            first_frame,
            ..
        } = *ctx;

        // On app exit, stop touching every plugin: `exit()` is about to run the
        // bundles' static destructors and any concurrent process call would race
        // them. Publish `in_process` and re-check so shutdown can't miss us.
        if self.shutdown.is_requested() {
            return;
        }
        self.shutdown.set_in_process(true);
        if self.shutdown.is_requested() {
            self.shutdown.set_in_process(false);
            return;
        }

        self.apply_commands();

        let block_start = first_frame as i64;
        let block_end = block_start + frames as i64;

        // Drain the clip feed into `pending`, turning each event's wall-clock
        // intent into an absolute frame in the engine's `steady` timeline. `At`
        // events get the constant one-buffer delay; `Immediate` events (seek,
        // stop note-offs) target the head of this block, or just after their
        // track's already-queued events (`immediate_target_frame`). Never
        // earlier than the block start — a straggler plays now, not in the past.
        while let Ok(event) = self.clip_midi_rx.pop() {
            let target = match event.when {
                EventTime::Immediate => {
                    immediate_target_frame(&self.pending, event.track, block_start)
                }
                EventTime::At(at) => ctx.scheduled_frame(at),
            };
            self.push_pending(target, event.track, event.message);
        }

        // Live keyboard events: no delay, head of the block. Same `pending`
        // path so note-on / note-off ordering against clip events is kept.
        while let Ok((track, bytes)) = self.live_midi_rx.pop() {
            self.push_pending(block_start, track, bytes);
        }

        self.pending
            .sort_unstable_by_key(|e| (e.target_frame, e.seq));

        // Events due in this block are a sorted prefix; the rest (held one
        // buffer ahead, or for a later callback) stay in `pending`.
        let due = self.pending.partition_point(|e| e.target_frame < block_end);
        for event in self.pending.drain(..due) {
            if let Some(Some(voice)) = self.voices.get_mut(event.track) {
                let within = within_block_offset(event.target_frame, block_start, frames);
                voice.queue_midi(event.message, within);
            }
        }

        let transport = self.transport.snapshot();
        let gains = self.target_gains();

        // --- Render pass ---
        // Each voice writes only its own output buffers and reads nothing
        // another voice writes, so these iterations are fully independent —
        // this is the loop that gets spread across worker threads. Everything
        // shared (the mix accumulator, the gain ramp) is deliberately left to
        // the summing pass below.
        //
        // A sleeping voice with nothing newly queued (and no process request,
        // see `wake_on_request`) is silent by definition — its contribution is
        // correctly zero without ever calling into the plugin — and a plugin
        // whose process call failed is treated as silent for this block. Both
        // record `rendered = false` for the summing pass.
        //
        // The awake count is what the pool sizes itself from: with one busy
        // voice (or none) it runs inline and never pays to wake a worker, and
        // with more it engages at most that many runners rather than one per
        // empty slot. Calling a plugin's process from a worker thread is legal
        // in both hosted formats — what they forbid is *concurrent* calls on
        // one instance, not a different thread per instance. See
        // `170-multicore-scheduling.md`.
        let mut awake = 0;
        for voice in self.voices.iter_mut().flatten() {
            voice.wake_on_request(frames);
            awake += usize::from(!voice.mix().sleeping);
        }
        ctx.pool
            .for_each(self.voices.as_mut_slice(), awake, |_track, slot| {
                if let Some(voice) = slot {
                    // A sleeping voice is never called into, so it isn't timed.
                    let (rendered, render_time) = if voice.mix().sleeping {
                        (false, Duration::ZERO)
                    } else {
                        let started = Instant::now();
                        let rendered = voice.render_block(frames, first_frame, &transport);
                        (
                            rendered,
                            if rendered {
                                started.elapsed()
                            } else {
                                Duration::ZERO
                            },
                        )
                    };
                    let m = voice.mix_mut();
                    m.rendered = rendered;
                    m.render_time = render_time;
                }
            });

        // Attribute the render pass per track, before the summing pass — this
        // is what the overrun journal reads to name the plugin that ate the
        // block. Every slot is written each block (an empty or skipped track
        // records zero) so a stale figure can never outlive its block.
        for (track, slot) in self.voices.iter().enumerate() {
            let render_time = slot
                .as_ref()
                .map_or(Duration::ZERO, |v| v.mix().render_time);
            ctx.load.record_track(track, render_time);
        }

        // --- Summing pass ---
        // Order-dependent, and writing one shared accumulator, so it stays on
        // this thread. A couple of multiply-adds per frame per track —
        // negligible next to the render pass above.
        let [left, right] = mix;
        for (track, slot) in self.voices.iter_mut().enumerate() {
            let Some(voice) = slot else { continue };
            let (target_l, target_r) = gains[track];
            // Ramp linearly across the block from last block's end gain to this
            // block's target, so a fader move (or a fresh voice's 0.0 start)
            // doesn't zipper.
            if voice.mix().rendered {
                let (mut gl, mut gr) = (voice.mix().gain_l, voice.mix().gain_r);
                let step_l = (target_l - gl) / frames as f32;
                let step_r = (target_r - gr) / frames as f32;
                for (f, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
                    gl += step_l;
                    gr += step_r;
                    *l += voice.sample(0, f) * gl;
                    *r += voice.sample(1, f) * gr;
                }
            }
            // The block ends at the target either way. A voice that rendered
            // nothing snaps straight to it, so it doesn't ramp from a stale
            // value when it wakes.
            let m = voice.mix_mut();
            m.gain_l = target_l;
            m.gain_r = target_r;
        }

        // Events queued this block have been consumed; the next starts empty.
        for voice in self.voices.iter_mut().flatten() {
            voice.clear_events();
        }

        self.shutdown.set_in_process(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(target_frame: i64, seq: u64, msg: Midi3) -> PendingEvent {
        PendingEvent {
            target_frame,
            track: 0,
            message: msg,
            seq,
        }
    }

    #[test]
    fn pending_sort_keeps_a_note_off_after_its_note_on_at_the_same_frame() {
        // Non-stable sort must not be free to reorder these — `seq` pins it.
        let mut evs = [
            pending(100, 7, [0x90, 60, 100]), // note-on, arrived first
            pending(100, 8, [0x80, 60, 0]),   // note-off, same frame, later
            pending(50, 9, [0x90, 62, 100]),
        ];
        evs.sort_unstable_by_key(|e| (e.target_frame, e.seq));
        let order: Vec<[u8; 3]> = evs
            .iter()
            .map(|e| [e.message[0], e.message[1], e.message[2]])
            .collect();
        assert_eq!(order, [[0x90, 62, 100], [0x90, 60, 100], [0x80, 60, 0]]);
    }

    #[test]
    fn immediate_event_lands_after_its_tracks_queued_note_on() {
        // A clip note-on still held one buffer ahead, then the stop's note-off.
        let mut evs = vec![pending(1_256, 0, [0x90, 60, 100])];
        let off_at = immediate_target_frame(&evs, 0, 1_000);
        assert_eq!(off_at, 1_256);
        evs.push(pending(off_at, 1, [0x80, 60, 0]));
        evs.sort_unstable_by_key(|e| (e.target_frame, e.seq));
        assert_eq!(evs[1].message, [0x80, 60, 0]);
    }

    #[test]
    fn immediate_event_ignores_other_tracks_and_past_events() {
        let mut evs = vec![pending(900, 0, [0x90, 60, 100])];
        let mut other = pending(5_000, 1, [0x90, 62, 100]);
        other.track = 1;
        evs.push(other);
        // Track 0 has only an overdue event; track 1's later one doesn't count.
        assert_eq!(immediate_target_frame(&evs, 0, 1_000), 1_000);
        assert_eq!(immediate_target_frame(&[], 0, 1_000), 1_000);
    }

    #[test]
    fn partition_point_selects_exactly_the_events_due_this_sub_block() {
        let evs = [
            pending(0, 0, [0x90, 60, 1]),
            pending(200, 1, [0x90, 61, 1]),
            pending(511, 2, [0x90, 62, 1]),
            pending(512, 3, [0x90, 63, 1]), // exactly at the boundary → next block
            pending(900, 4, [0x90, 64, 1]),
        ];
        let sub_end = 512;
        assert_eq!(evs.partition_point(|e| e.target_frame < sub_end), 3);
    }

    #[test]
    fn within_block_offset_rebases_and_clamps() {
        // In-range: offset relative to the sub-block start.
        assert_eq!(within_block_offset(600, 512, 256), 88);
        // Overdue (target before the sub-block): offset 0.
        assert_eq!(within_block_offset(400, 512, 256), 0);
        // Never reaches `frames` — the formats need offset < frames.
        assert_eq!(within_block_offset(10_000, 512, 256), 255);
    }
}
