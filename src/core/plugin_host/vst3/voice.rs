//! One hosted VST3 instrument as the audio thread sees it.
//!
//! [`Vst3Voice`] is the `Send` half of a loaded plugin: the `IAudioProcessor`,
//! the reusable audio buffers, and the per-block event list. The `!Send` half —
//! the component and edit controller — stays on the main thread in
//! [`Vst3Editor`](super::editor::Vst3Editor).
//!
//! Structurally this is the CLAP voice's twin, and deliberately so: the mixer
//! calls exactly the same [`InstrumentVoice`] methods on both. What differs is
//! underneath — `process` takes one flat `ProcessData` rather than typed port
//! builders, MIDI has to become typed events (see [`events`](super::events)),
//! and there is no `ProcessStatus::Sleep` to tell us when to stop.
//!
//! See `docs/180-vst3-host.md`.

use std::ptr::null_mut;

use vst3::ComPtr;
use vst3::ComWrapper;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kCycleActive, kPlaying, kProjectTimeMusicValid, kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IEventList,
    IParameterChanges, ProcessContext, ProcessData,
};
use vst3::Steinberg::Vst::{ProcessModes_::kRealtime, SymbolicSampleSizes_::kSample32};
use vst3::Steinberg::kResultOk;

use crate::core::config::MAX_TRACKS;
use crate::core::midi::message::Midi3;
use crate::core::plugin_host::buffers::{AudioIoLayout, PortBuffers, channel_total};
use crate::core::plugin_host::transport::BlockTransport;
use crate::core::plugin_host::voice::{EVENT_CAPACITY, InstrumentVoice, VoiceMix};
use crate::core::time::ticks_to_beats_f64;

use super::events::{MidiMap, NoteIds, Translated, translate};
use super::host::HostEventList;
use super::params::ParamReceiver;

/// Most audio buses a voice can hand `process` in one direction. Generous for
/// an instrument: multi-out samplers top out around 16 stereo buses.
const MAX_BUSES: usize = 64;

/// Most audio channels a voice can hand `process` in one direction, summed
/// across all of that direction's buses.
const MAX_CHANNELS: usize = 256;

/// An unfilled slot in a per-block bus array.
const EMPTY_BUS: AudioBusBuffers = AudioBusBuffers {
    numChannels: 0,
    silenceFlags: 0,
    __field0: AudioBusBuffers__type0 {
        channelBuffers32: null_mut(),
    },
};

/// The **floor** on how many consecutive silent samples a voice must produce
/// before it may sleep, on top of whatever tail the plugin declares.
///
/// VST3 has **no `ProcessStatus::Sleep`** — nothing in the API says "I am done
/// until you send me something" — so idling has to be inferred. Two things
/// decide it together:
///
/// - the plugin's own `getTailSamples`, which is the authoritative answer to
///   "how much longer might I still produce output", and
/// - this floor, which stops a voice thrashing in and out of sleep across the
///   gaps in a busy part.
///
/// Roughly a fifth of a second at 48 kHz. It is deliberately *not* the tail
/// estimate — [`Vst3Voice::tail_samples`] is.
const MIN_SILENT_SAMPLES: u64 = 10_240;

/// Sleeping samples between heartbeats — one empty `process` call, so a
/// plugin that advances internal state per call does not freeze mid-way and
/// finish under the next note. About 85 ms at 48 kHz. See "There is no
/// `ProcessStatus::Sleep`" in `180-vst3-host.md`.
const HEARTBEAT_SAMPLES: u64 = 4_096;

/// `getTailSamples`' sentinel for "this never settles" — a plugin with an
/// infinite reverb or a self-oscillating filter. Such a voice is never slept.
const INFINITE_TAIL: u32 = u32::MAX;

/// One hosted VST3 instrument: its audio processor plus the reusable buffers
/// and event list. MIDI is pushed in by the mixer before each block.
pub(super) struct Vst3Voice {
    /// The plugin's audio processor. Only ever called from the audio callback
    /// thread — see [`LoadedPlugin::processor`](super::component::LoadedPlugin).
    processor: ComPtr<IAudioProcessor>,
    /// This block's note events, read by the plugin inside `process`.
    events: ComWrapper<HostEventList>,
    /// The audio-thread end of the UI→processor parameter bridge, drained into
    /// an `IParameterChanges` before each block. For a single-component plugin
    /// this is always empty and costs one ring pop; for a dual-component one it
    /// is the only way a UI change reaches the sound. See
    /// [`params`](super::params).
    params: ParamReceiver,
    /// The silent input feed and the output audio, `[bus][channel]`. See
    /// [`PortBuffers`].
    bufs: PortBuffers,
    /// Note-on ↔ note-off id pairing. See [`NoteIds`].
    note_ids: NoteIds,
    /// Which parameter each MIDI controller drives on this plugin. Built once
    /// at load; VST3 has no other way to receive a CC. See
    /// [`MidiMap`].
    midi_map: MidiMap,
    /// Consecutive fully-silent samples produced so far. Counted in samples
    /// rather than blocks so it can be compared against
    /// [`tail_samples`](Self::tail_samples) directly, and so a changed buffer
    /// size does not change the timing.
    silent_samples: u64,
    /// Samples slept towards the next heartbeat. See [`HEARTBEAT_SAMPLES`].
    asleep_samples: u64,
    /// Where [`asleep_samples`](Self::asleep_samples) restarts on a wake,
    /// spread by track so voices that fall asleep together (every one, after
    /// a transport stop) don't all beat in the same block.
    heartbeat_phase: u64,
    /// How much output the plugin says it may still produce after its last
    /// input (`getTailSamples`), and therefore how long it must stay awake
    /// after falling silent. [`INFINITE_TAIL`] means never sleep.
    tail_samples: u32,
    /// The mixer-owned render flags and gain ramp.
    mix: VoiceMix,
    /// The device sample rate this plugin was activated against — fixed for
    /// the voice's lifetime, and carried into every block's `ProcessContext`
    /// (see [`process_context`]).
    sample_rate: f64,
}

impl Vst3Voice {
    /// Wraps an activated processor into a ready voice with buffers matching
    /// the plugin's declared bus layout.
    pub(super) fn new(
        processor: ComPtr<IAudioProcessor>,
        max_frames: usize,
        io: &AudioIoLayout,
        params: ParamReceiver,
        midi_map: MidiMap,
        tail_samples: u32,
        sample_rate: f64,
    ) -> Self {
        Self {
            processor,
            events: ComWrapper::new(HostEventList::new(EVENT_CAPACITY)),
            params,
            bufs: PortBuffers::new(io, max_frames),
            note_ids: NoteIds::default(),
            midi_map,
            silent_samples: 0,
            asleep_samples: 0,
            heartbeat_phase: 0,
            tail_samples,
            mix: VoiceMix::default(),
            sample_rate,
        }
    }

    /// Spreads this voice's heartbeats by `track` — see
    /// [`heartbeat_phase`](Self::heartbeat_phase).
    pub(super) fn staggered_for(mut self, track: usize) -> Self {
        self.heartbeat_phase = (track % MAX_TRACKS) as u64 * HEARTBEAT_SAMPLES / MAX_TRACKS as u64;
        self.asleep_samples = self.heartbeat_phase;
        self
    }
}

impl InstrumentVoice for Vst3Voice {
    fn queue_midi(&mut self, bytes: Midi3, time: u32) {
        match translate(bytes, time, &mut self.note_ids, &self.midi_map) {
            Translated::Event(event) => self.events.push(event),
            // A CC lands in this block's automation queue rather than its event
            // list — VST3 has no other way to carry one. `clear_events` empties
            // both at the end of the block.
            Translated::Param(id, value) => self.params.push(id, value),
            Translated::Nothing => return,
        }
        self.wake();
    }

    fn render_block(&mut self, frames: usize, _steady: u64, transport: &BlockTransport) -> bool {
        self.bufs.prepare_outputs(frames);

        let Some(events) = self.events.to_com_ptr::<IEventList>() else {
            return false;
        };
        // The UI's changes join whatever CCs `queue_midi` already put in this
        // block's queue.
        self.params.drain_ui();
        let Some(params) = self.params.changes().to_com_ptr::<IParameterChanges>() else {
            return false;
        };

        // Rebuilt per block in stack arrays, never cached on the voice and
        // never allocated — see "Rendering a block" in `180-vst3-host.md`.
        let mut in_ptrs = [null_mut(); MAX_CHANNELS];
        let mut out_ptrs = [null_mut(); MAX_CHANNELS];
        let mut in_buses = [EMPTY_BUS; MAX_BUSES];
        let mut out_buses = [EMPTY_BUS; MAX_BUSES];
        let num_inputs = fill_buses(&mut self.bufs.inputs, &mut in_ptrs, &mut in_buses);
        let num_outputs = fill_buses(&mut self.bufs.outputs, &mut out_ptrs, &mut out_buses);
        let mut context = process_context(transport, self.sample_rate);

        let mut data = ProcessData {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            numSamples: frames as i32,
            numInputs: num_inputs as i32,
            numOutputs: num_outputs as i32,
            inputs: in_buses.as_mut_ptr(),
            outputs: out_buses.as_mut_ptr(),
            inputParameterChanges: params.as_ptr(),
            outputParameterChanges: std::ptr::null_mut(),
            inputEvents: events.as_ptr(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut context,
        };

        // SAFETY: `data` describes buffers this voice owns and keeps alive for
        // the whole call — the channel-pointer arrays, the bus descriptors, the
        // context and the two COM objects are all locals that outlive it. Every
        // channel is at least `frames` long. The VST3 spec scopes `ProcessData`
        // to this invocation, so nothing here may be retained past it.
        let ok = unsafe { self.processor.process(&mut data) } == kResultOk;

        if ok {
            self.update_idle_state(frames);
        }
        ok
    }

    fn sample(&self, channel: usize, frame: usize) -> f32 {
        self.bufs.main_sample(channel, frame)
    }

    fn clear_events(&mut self) {
        self.events.clear();
        // The automation queue is emptied here, at the *end* of the block,
        // rather than in `render_block` — CCs are pushed into it during the
        // mixer's event dispatch, which happens first, and resetting later
        // would throw them away.
        self.params.reset();
    }

    /// Wakes the voice while UI parameter changes wait in the ring — a knob
    /// turned in a dual-component plugin's editor, or the rest of a preset
    /// burst `drain_ui` carried over — so they reach the processor without
    /// waiting for the next note. Otherwise a sleeping voice gets its
    /// [heartbeat](HEARTBEAT_SAMPLES), with the silence count left alone so
    /// [`update_idle_state`](Self::update_idle_state) decides afresh.
    fn wake_on_request(&mut self, frames: usize) {
        if self.params.ui_pending() {
            self.wake();
        } else if self.mix.sleeping {
            self.asleep_samples = self.asleep_samples.saturating_add(frames as u64);
            if self.asleep_samples >= HEARTBEAT_SAMPLES {
                self.asleep_samples = 0;
                self.mix.sleeping = false;
            }
        }
    }

    fn mix(&self) -> &VoiceMix {
        &self.mix
    }

    fn mix_mut(&mut self) -> &mut VoiceMix {
        &mut self.mix
    }
}

impl Vst3Voice {
    /// Brings a sleeping voice back and restarts its silence count, so it
    /// stays awake at least the floor after whatever woke it.
    fn wake(&mut self) {
        self.mix.sleeping = false;
        self.silent_samples = 0;
        self.asleep_samples = self.heartbeat_phase;
    }

    /// Decides whether this voice may sleep, from what it just rendered.
    ///
    /// Sleeping means `process` is not called again until an event arrives, so
    /// getting it wrong does not merely waste a block — **any output the plugin
    /// would have produced on its own is lost.** A feedback delay whose taps are
    /// separated by digital silence is the shape that would break, which is why
    /// the plugin's declared tail is respected rather than guessed at.
    fn update_idle_state(&mut self, frames: usize) {
        if self.bufs.main_output_silent(frames) && self.events.is_empty() {
            self.silent_samples = self.silent_samples.saturating_add(frames as u64);
            self.mix.sleeping = may_sleep(self.silent_samples, self.tail_samples);
        } else {
            self.silent_samples = 0;
        }
    }
}

/// Rejects a bus layout too large for the fixed per-block arrays
/// [`render_block`](InstrumentVoice::render_block) fills, so the check happens
/// once at load instead of truncating buses on the audio thread.
pub(super) fn check_bus_capacity(io: &AudioIoLayout) -> Result<(), String> {
    for (dir, buses) in [("input", &io.inputs), ("output", &io.outputs)] {
        let channels = channel_total(buses);
        if buses.len() > MAX_BUSES || channels > MAX_CHANNELS {
            return Err(format!(
                "{} {dir} buses / {channels} channels exceeds the host's \
                 {MAX_BUSES} / {MAX_CHANNELS}",
                buses.len()
            ));
        }
    }
    Ok(())
}

/// Points `buses[i]` at bus `i` of `bufs`, packing every bus's channel
/// pointers into `ptrs` back to back. Returns the number of buses filled.
/// Writes in place and never panics: a layout too big for the arrays (which
/// [`check_bus_capacity`] rejects at load) is cut short.
fn fill_buses(
    bufs: &mut [Vec<Vec<f32>>],
    ptrs: &mut [*mut f32],
    buses: &mut [AudioBusBuffers],
) -> usize {
    let mut next = 0;
    for (filled, (bus, slot)) in bufs.iter_mut().zip(buses.iter_mut()).enumerate() {
        let Some(channels) = ptrs.get_mut(next..next + bus.len()) else {
            return filled;
        };
        next += bus.len();
        for (ptr, channel) in channels.iter_mut().zip(bus.iter_mut()) {
            *ptr = channel.as_mut_ptr();
        }
        *slot = AudioBusBuffers {
            numChannels: channels.len() as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: channels.as_mut_ptr(),
            },
        };
    }
    bufs.len().min(buses.len())
}

/// Builds the VST3 transport context for a block, from the format-neutral
/// snapshot the mixer took. 4/4 is assumed, matching the rest of the app.
///
/// `sample_rate` has no validity flag of its own — unlike every other field
/// here, the spec treats it as always required, not something a plugin may
/// only trust when a `state` bit says so. Leaving it zeroed (as `mem::zeroed`
/// would) invites exactly the kind of internal divide-by-zero this host has
/// hit: a plugin computing samples from musical time via the sample rate it
/// was just handed.
fn process_context(t: &BlockTransport, sample_rate: f64) -> ProcessContext {
    let mut state = kTempoValid | kTimeSigValid | kProjectTimeMusicValid;
    if t.running {
        state |= kPlaying;
    }
    if t.looping {
        state |= kCycleActive;
    }
    // SAFETY: `ProcessContext` is a plain `repr(C)` struct of scalars; zeroing
    // it is the documented way to start from "nothing valid" before setting the
    // fields whose validity flags are raised above.
    let mut context: ProcessContext = unsafe { std::mem::zeroed() };
    context.state = state;
    context.sampleRate = sample_rate;
    context.projectTimeMusic = ticks_to_beats_f64(t.playback_tick);
    context.barPositionMusic = t.bar_start_beats();
    context.cycleStartMusic = ticks_to_beats_f64(t.region_start);
    context.cycleEndMusic = ticks_to_beats_f64(t.region_end);
    context.tempo = t.bpm();
    context.timeSigNumerator = t.meter.numerator().into();
    context.timeSigDenominator = t.meter.denominator().into();
    context
}

/// Whether a voice that has produced `silent_samples` of digital silence may
/// stop being processed.
///
/// It must have been silent for **both** the plugin's declared tail — after
/// which the plugin itself says nothing more can come — and a floor that stops
/// it thrashing across short gaps. A plugin declaring an infinite tail never
/// sleeps.
fn may_sleep(silent_samples: u64, tail_samples: u32) -> bool {
    if tail_samples == INFINITE_TAIL {
        return false;
    }
    silent_samples >= MIN_SILENT_SAMPLES.max(u64::from(tail_samples))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use vst3::Steinberg::Vst::{
        BusDirection, IAudioProcessorTrait, IEventListTrait, ProcessSetup, SpeakerArrangement,
    };
    use vst3::Steinberg::{TBool, int32, tresult, uint32};
    use vst3::{Class, ComRef};

    use super::*;
    use crate::core::plugin_host::vst3::params::bridge;
    use crate::core::time::{Meter, PPQN};

    /// Block size the fake-processor tests render at.
    const FRAMES: usize = 512;

    /// A silent stand-in processor that logs how many events each `process`
    /// call was handed.
    struct FakeProcessor {
        /// One entry per `process` call: that call's event count.
        calls: Arc<Mutex<Vec<i32>>>,
    }

    impl Class for FakeProcessor {
        type Interfaces = (IAudioProcessor,);
    }

    impl IAudioProcessorTrait for FakeProcessor {
        unsafe fn setBusArrangements(
            &self,
            _inputs: *mut SpeakerArrangement,
            _num_ins: int32,
            _outputs: *mut SpeakerArrangement,
            _num_outs: int32,
        ) -> tresult {
            kResultOk
        }
        unsafe fn getBusArrangement(
            &self,
            _dir: BusDirection,
            _index: int32,
            _arr: *mut SpeakerArrangement,
        ) -> tresult {
            kResultOk
        }
        unsafe fn canProcessSampleSize(&self, _size: int32) -> tresult {
            kResultOk
        }
        unsafe fn getLatencySamples(&self) -> uint32 {
            0
        }
        unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
            kResultOk
        }
        unsafe fn setProcessing(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn process(&self, data: *mut ProcessData) -> tresult {
            // SAFETY: the voice hands a valid `ProcessData` whose event list
            // outlives this call.
            let count = unsafe {
                ComRef::from_raw((*data).inputEvents).map_or(-1, |events| events.getEventCount())
            };
            self.calls.lock().unwrap().push(count);
            kResultOk
        }
        unsafe fn getTailSamples(&self) -> uint32 {
            0
        }
    }

    /// A stereo voice around a [`FakeProcessor`], plus that processor's log.
    fn fake_voice() -> (Vst3Voice, Arc<Mutex<Vec<i32>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let processor = ComWrapper::new(FakeProcessor {
            calls: Arc::clone(&calls),
        })
        .to_com_ptr::<IAudioProcessor>()
        .unwrap();
        let (_tx, rx) = bridge();
        let voice = Vst3Voice::new(
            processor,
            FRAMES,
            &AudioIoLayout::new(Vec::new(), vec![2]),
            rx,
            MidiMap::empty(),
            0,
            48_000.0,
        );
        (voice, calls)
    }

    /// A stopped 4/4 transport at 120 BPM, one bar of loop region.
    fn stopped_transport() -> BlockTransport {
        BlockTransport {
            running: false,
            looping: false,
            tempo_us: 500_000,
            meter: Meter::FOUR_FOUR,
            playback_tick: 0,
            region_start: 0,
            region_end: PPQN * 4,
        }
    }

    /// One block the way the mixer runs it: the wake check, a render if the
    /// voice is awake, then clear.
    fn run_block(voice: &mut Vst3Voice) {
        voice.wake_on_request(FRAMES);
        if voice.mix().sleeping {
            return;
        }
        assert!(voice.render_block(FRAMES, 0, &stopped_transport()));
        voice.clear_events();
    }

    /// Renders silent blocks until the voice falls asleep.
    fn sleep(voice: &mut Vst3Voice) {
        while !voice.mix().sleeping {
            run_block(voice);
        }
    }

    #[test]
    fn a_sleeping_voice_gets_a_silent_heartbeat_and_sleeps_again() {
        // Omnisphere's first load in a session: work it advances per `process`
        // call must keep moving while the voice is silent, or the first note
        // plays the rest of it (a short high-pitched burst).
        let (mut voice, calls) = fake_voice();
        sleep(&mut voice);
        calls.lock().unwrap().clear();

        let per_beat = (HEARTBEAT_SAMPLES as usize).div_ceil(FRAMES);
        for _ in 0..per_beat * 3 {
            run_block(&mut voice);
        }
        assert_eq!(*calls.lock().unwrap(), vec![0, 0, 0]);
        assert!(
            voice.mix().sleeping,
            "a silent heartbeat goes back to sleep"
        );
    }

    #[test]
    fn voices_that_fall_asleep_together_beat_in_different_blocks() {
        // After a transport stop every voice sleeps in the same block; their
        // heartbeats must not all land in one block and wake the worker pool.
        let beat_blocks = |track| {
            let (voice, calls) = fake_voice();
            let mut voice = voice.staggered_for(track);
            sleep(&mut voice);
            calls.lock().unwrap().clear();
            let mut beats = Vec::new();
            for block in 0..(HEARTBEAT_SAMPLES as usize).div_ceil(FRAMES) * 2 {
                let before = calls.lock().unwrap().len();
                run_block(&mut voice);
                if calls.lock().unwrap().len() > before {
                    beats.push(block);
                }
            }
            beats
        };
        let (a, b) = (beat_blocks(0), beat_blocks(MAX_TRACKS / 2));
        assert_eq!((a.len(), b.len()), (2, 2));
        assert!(a.iter().all(|block| !b.contains(block)), "{a:?} vs {b:?}");
    }

    #[test]
    fn fill_buses_packs_each_bus_into_the_flat_pointer_array() {
        // A multi-out layout: stereo main, mono aux, stereo aux.
        let io = AudioIoLayout::new(Vec::new(), vec![2, 1, 2]);
        let mut bufs = PortBuffers::new(&io, 16);
        let mut ptrs = [null_mut(); MAX_CHANNELS];
        let mut buses = [EMPTY_BUS; MAX_BUSES];

        let n = fill_buses(&mut bufs.outputs, &mut ptrs, &mut buses);

        assert_eq!(n, 3);
        let mut next = 0;
        for (bus, channels) in buses[..n].iter().zip(&mut bufs.outputs) {
            assert_eq!(bus.numChannels as usize, channels.len());
            // SAFETY: reading the union field `fill_buses` just wrote.
            let base = unsafe { bus.__field0.channelBuffers32 };
            assert_eq!(base, ptrs[next..].as_mut_ptr());
            for channel in channels.iter_mut() {
                assert_eq!(ptrs[next], channel.as_mut_ptr());
                next += 1;
            }
        }
    }

    #[test]
    fn fill_buses_stops_rather_than_overrunning_the_arrays() {
        let io = AudioIoLayout::new(Vec::new(), vec![2, 2, 2]);
        let mut bufs = PortBuffers::new(&io, 16);
        let mut ptrs = [null_mut(); 3];
        let mut buses = [EMPTY_BUS; 2];
        // Only the first bus fits in 3 channel slots alongside the second's 2.
        assert_eq!(fill_buses(&mut bufs.outputs, &mut ptrs, &mut buses), 1);
    }

    #[test]
    fn a_layout_past_the_per_block_arrays_is_rejected_at_load() {
        let fits = AudioIoLayout::new(vec![2], vec![2; 16]);
        assert!(check_bus_capacity(&fits).is_ok());

        let too_many_buses = AudioIoLayout::new(Vec::new(), vec![1; MAX_BUSES + 1]);
        assert!(check_bus_capacity(&too_many_buses).is_err());

        let too_many_channels = AudioIoLayout::new(vec![u16::MAX], vec![2]);
        assert!(check_bus_capacity(&too_many_channels).is_err());
    }

    #[test]
    fn a_voice_sleeps_only_after_its_declared_tail_has_passed() {
        // The floor alone is not enough for a plugin with a long tail: sleeping
        // early would lose a delay tap that arrives after a silent gap.
        let long_tail: u32 = 48_000; // one second at 48 kHz
        assert!(!may_sleep(MIN_SILENT_SAMPLES, long_tail));
        assert!(!may_sleep(u64::from(long_tail) - 1, long_tail));
        assert!(may_sleep(u64::from(long_tail), long_tail));
    }

    #[test]
    fn a_short_tail_still_waits_out_the_floor() {
        // Most instruments report no tail at all; without a floor they would
        // sleep and wake on every gap between notes.
        assert!(!may_sleep(1, 0));
        assert!(!may_sleep(MIN_SILENT_SAMPLES - 1, 0));
        assert!(may_sleep(MIN_SILENT_SAMPLES, 0));
    }

    #[test]
    fn an_infinite_tail_never_sleeps() {
        // A self-oscillating filter or an infinite reverb: the plugin has said
        // it may produce output at any time, so it must keep being called.
        assert!(!may_sleep(u64::MAX, INFINITE_TAIL));
    }

    #[test]
    fn the_process_context_carries_tempo_position_and_loop_bounds() {
        let t = BlockTransport {
            running: true,
            looping: true,
            tempo_us: 500_000,
            meter: Meter::FOUR_FOUR,
            playback_tick: PPQN * 8,
            region_start: PPQN * 4,
            region_end: PPQN * 8,
        };
        let c = process_context(&t, 48_000.0);
        assert!((c.tempo - 120.0).abs() < 1e-9);
        assert!((c.projectTimeMusic - 8.0).abs() < 1e-9);
        assert!((c.barPositionMusic - 8.0).abs() < 1e-9);
        assert!((c.cycleStartMusic - 4.0).abs() < 1e-9);
        assert!((c.cycleEndMusic - 8.0).abs() < 1e-9);
        assert_eq!(c.timeSigNumerator, 4);
        assert_eq!(c.sampleRate, 48_000.0);
    }

    #[test]
    fn the_process_context_carries_the_project_meter() {
        // Bar 2 of 7/8 starts 7 quarters in.
        let t = BlockTransport {
            running: true,
            looping: false,
            tempo_us: 500_000,
            meter: Meter::new(7, 8).unwrap(),
            playback_tick: PPQN * 8,
            region_start: 0,
            region_end: PPQN * 8,
        };
        let c = process_context(&t, 48_000.0);
        assert_eq!(c.timeSigNumerator, 7);
        assert_eq!(c.timeSigDenominator, 8);
        assert!((c.barPositionMusic - 7.0).abs() < 1e-9);
    }

    #[test]
    fn process_context_state_flags_follow_running_and_looping() {
        let base = stopped_transport();
        let stopped = process_context(&base, 48_000.0);
        assert_eq!(stopped.state & kPlaying, 0);
        assert_eq!(stopped.state & kCycleActive, 0);
        // The validity flags are unconditional — they say which fields we filled
        // in, not what the transport is doing.
        assert_ne!(stopped.state & kTempoValid, 0);
        assert_ne!(stopped.state & kProjectTimeMusicValid, 0);

        let rolling = process_context(
            &BlockTransport {
                running: true,
                looping: true,
                ..base
            },
            48_000.0,
        );
        assert_ne!(rolling.state & kPlaying, 0);
        assert_ne!(rolling.state & kCycleActive, 0);
    }
}
