# Audio Settings and Following the Device

**Status: design agreed 2026-11-13; phase 1 built (the engine restarts, and plugins are reactivated in place on a rate change), awaiting the hand tests on the plugin list. Issue: #31. Replaces the device half of issue #9.** Settings gets an **Audio** tab, where you pick the output device, sample rate and buffer size and see what the engine is actually running at. The engine stops being fixed at launch. A change from the tab, or from outside (Audio MIDI Setup, a device unplugged, the macOS default changing), goes through one **audio restart**: the stream and the worker pool are rebuilt against the device as it is now, and the plugins are reactivated if the rate changed. Because the pool is rebuilt, the workers re-join the new device's workgroup each time, which is the cancelled-workgroup bug in #9.

## Background

Today the device is chosen once, at launch, and never looked at again (`AudioEngine::start`, `src/core/audio/engine.rs`):

- **Device:** `default_output_device()`. There's no way to pick another one short of changing the macOS default before launching.
- **Sample rate:** whatever `default_output_config()` reports, usually the device's current nominal rate.
- **Buffer size:** `DESIRED_BUFFER_FRAMES` (256), clamped to the device's range and probed with a throwaway stream; `BufferSize::Default` if the driver refuses.
- **After that** the `"audio-engine"` thread parks for the life of the app. The stream's error callback counts errors and logs them, and nothing acts on them.

What cpal 0.18 (`host/coreaudio/macos`) does underneath when something changes:

| Event | Stream opened on the *default* device (Stev today) | Stream opened on a *specific* device |
|---|---|---|
| macOS default output changes (headphones in, AirPods connect) | The AudioUnit re-routes by itself, reports `ErrorKind::DeviceChanged`, keeps playing | Nothing; keeps playing on its device |
| No default device left | Paused, `DeviceNotAvailable` | — |
| The device is unplugged | (re-routes as above) | Paused, `DeviceNotAvailable` |
| Its sample rate changes (Audio MIDI Setup, another app) | Nothing reported; the AudioUnit converts to the rate the stream was built at | **Paused**, `StreamInvalidated` |
| Its buffer size changes | Nothing reported | Nothing reported |

So with the default device Stev keeps making sound through every one of these, and that's why #9 looked harmless. What goes stale in each case:

- **The workgroup.** The workers joined the *opened* device's IO workgroup (`170` § Audio workgroup). After a re-route they're in the old device's group, or one CoreAudio cancelled, for the rest of the session.
- **The rate.** After a re-route to a device running at another rate, or a rate change in Audio MIDI Setup, every block goes through CoreAudio's converter. It works, but it isn't the rate the user set, and nothing says so.
- **The buffer.** The 256 was set on the old device. The new one runs at its own buffer, while `SCHEDULE_DELAY_FRAMES` stays one 256-frame buffer, so scheduled events can land late (`RenderCtx::scheduled_frame`).
- **What the app reports.** The overrun journal's budget, the DSP meter's deadline (`AudioLoad::record` takes the rate) and the export's rate all assume the launch configuration.

There is also no way at all to choose a device, a rate or a buffer. Every DAW and most standalone instruments have this tab, and a public release is expected to have it.

## Decisions

1. **An Audio tab, first in Settings.** Sound is the first thing a new user checks, so the tab goes before MIDI, and the modal opens on it (`SettingsTab::default`). It shows:
   - **Output device:** a list in the MIDI tab's `PortList` style. Its first row is **System default** ("System default — MacBook Pro Speakers"), then every output device by name. The row in use carries the badge.
   - **Sample rate:** the device's rates (`kAudioDevicePropertyAvailableNominalSampleRates` through cpal's supported configs), as a row of choices: 44.1, 48, 88.2, 96 kHz and so on.
   - **Buffer size:** 32, 64, 128, 256, 512, 1024, 2048, limited to the device's range and to `MAX_FRAMES`. Each is labelled with its length in ms at the current rate ("256 (5.3 ms)").
   - **A readout of what is running:** "MacBook Pro Speakers · 48 kHz · 256 frames · output latency 11.2 ms". Latency is buffer + device latency + safety offset, as cpal works it out for `OutputCallbackInfo::timestamp().playback`. When the device doesn't give what was asked for, the readout says so in fg_dim ("asked for 64; the device runs at 512").

   Output only: Stev has no audio input (`280` § Not planned: audio recording). Channels stay as they are (L/R on outputs 1–2, the rest silent); choosing an output pair on a multichannel interface is a later candidate (§ Not in this design).
2. **Choosing is applying.** Picking a row applies it at once, with no Apply button, as the MIDI tab connects a port when you pick it. The audio drops out for the length of a restart (decision 5).
3. **Stev sets the device's rate, as a DAW does.** Picking 96 kHz sets the device's nominal rate (cpal does this when it builds the stream), which other apps on the device hear too. **The default is "the device's rate":** until the user picks one, Stev opens at whatever the device runs at and never changes it. The rate list marks that choice ("48 kHz (device)"). Once chosen, a rate is kept per app, not per device. A device that can't run the saved rate opens at its own, and the readout says so.
4. **Saved in settings, not in the project.** `SettingsData` gains three fields, all `#[serde(default)]` so an older file means "as today":
   - `audio_output_device: Option<String>`: the device's persistent ID (cpal's `Device::id`, the CoreAudio UID), with its name alongside for display. `None` is System default.
   - `audio_sample_rate: Option<u32>`: `None` is the device's rate (decision 3).
   - `audio_buffer_frames: u32`: default 256, today's value.

   A project file never stores a rate. Clips are in ticks and plugin state doesn't depend on the rate, so a project opens the same at any rate. A saved device that isn't connected at launch falls back to System default. The list shows it as wanted but missing ("Scarlett 2i2 (not connected)") and **Stev switches to it when it appears**, as the MIDI tab reconnects a port that comes back.
5. **One restart path for every change.** The `"audio-engine"` thread stops parking and waits on a channel. Every change becomes one `EngineCommand::Restart` there: a choice in the tab, or the stream's error callback reporting `DeviceChanged`, `DeviceNotAvailable` or `StreamInvalidated`. The thread then:
   1. drops the stream, and with it gets the `Mixer` back (decision 6);
   2. resolves the target: the chosen device, or the current default, or the default as a fallback when the chosen one is gone;
   3. rebuilds the `WorkerPool`, so every worker joins the **new** device's workgroup. This is what closes #9;
   4. builds and starts the stream at the requested rate and buffer, probing the buffer as today;
   5. reports what it got (`UiEvent::AudioConfig { device, rate, buffer, latency_ms }`) to the tab, the footer and the sources (decision 7).

   Restarts are coalesced: if commands pile up while one runs (a device being plugged in fires several), only the last is acted on.
6. **The mixer outlives its stream.** The `Mixer` (sources, the clock, scratch, the journal writer) moves into the data callback today and dies with the stream. The callback will own it through a wrapper that sends it back to the engine thread when the closure drops, after CoreAudio has stopped calling it. The sources keep their state across a restart: the instrument voices, the click, the export tap.
7. **A new rate reaches every source; a new buffer reaches only the scheduler.**
   - **A buffer change** needs nothing from the plugins: voices are built for `MAX_FRAMES` (4096) whatever the device buffer. `SCHEDULE_DELAY_FRAMES` stops being a constant and becomes one buffer of the *running* stream, handed to sources in `RenderCtx`.
   - **A rate change** reaches the sources through a new `AudioSource::set_sample_rate`, run on the engine thread while the stream is down: `AudioClock`, the click voice, the meters' `BlockDecay`, the export tap. The plugins need more than that (decision 8).
8. **Plugins are reactivated in place on a rate change, not reloaded.** Both formats support this without a new instance: CLAP deactivates and activates again with a new rate, and VST3 takes `setupProcessing` while inactive (`setActive(false)` → `setupProcessing` → `setActive(true)`). Instances, open editors and state all survive. The sequence:
   1. the engine thread, holding the mixer while the stream is down, takes every voice out of the instrument mixer and sends them to the UI (as the reclaim path sends removed voices off the audio thread, except these are handed back, not dropped);
   2. the stream is started at the new rate *without* those voices, so the click and MIDI Out keep working;
   3. the UI reactivates one voice per frame under the restore panel already used for opening a project (`130` § Project persistence), titled "Restarting audio", naming the plugin and the step ("3 of 8");
   4. once every voice is back, they're inserted into the mixer in one go, so the tracks come back together. A voice that fails to reactivate leaves its track silent, as a plugin that fails to load does today.

   *As built (phase 1):* the UI asks for the voices back rather than the engine thread sending them, and each goes back in as it is reactivated, not all together (§ Things to check: Reactivating in place).

   The alternative, saving each plugin's state and loading it again through the project-open staging, reuses a tested path, but it is slower (Omnisphere-sized plugins take seconds to instantiate) and closes the editors. It's kept as the fallback for a format that turns out to need it (§ Things to check).
9. **The transport keeps running through a restart.** A restart isn't an edit and doesn't move the playhead (`220` principle 2). The sequencer keeps time on its own clock, so the loop carries on, MIDI Out included, and audio comes back in step. Every voice is sent all-notes-off as it goes back into the mixer, so nothing hangs from a note-off lost during the drop-out.
10. **The audio export comes first.** While an export runs, the tab is read-only and says why ("Exporting audio…"). An outside change during an export ends it, as the device disappearing does today: the writer already counts stream errors. Nothing restarts the engine under a running export.
11. **Every restart says what happened, in the footer, in fg/fg_dim.** A choice in the tab gets no extra line (the readout changes in front of you). An outside one does: "Audio switched to AirPods Pro (System default changed)", "Scarlett 2i2 disconnected; using MacBook Pro Speakers", "Device rate changed to 44.1 kHz". It's a line, not a dialog, and no alarm colour (`no-alarm-colours-in-ui`).
12. **The journal and the meter follow.** Each restart starts a new session header in `audio-overruns.log` with the new shape and its workgroup count. `AudioLoad`'s budget follows the running buffer and rate.
13. **Other platforms get the tab too.** Device, rate and buffer go through cpal, so the tab works off macOS. What stays macOS-only is what already is: the workgroup and the plugin reactivation, behind the same `cfg`s as now. The outside-change triggers are whatever cpal reports there.

## Not in this design

- **Choosing the output channels** on a multichannel interface (outputs 3–4, say). This is the likeliest follow-up for interface users, so it goes in `280` as a candidate once this is built.
- **Audio input** of any kind (`280` § Not planned).
- **An export rate of its own.** The stems are written at the running rate, as now.
- **Deriving the MIDI-Out offset.** `160` defaults the offset to one 256-frame buffer and leaves the device's latency to the user's ear. With the buffer now variable and the latency now known, the offset could become "the plugin path's latency + a user trim". That changes what the setting means for everyone who has dialled it in, so it's a design of its own. Until then, the tab's readout gives the user the number to dial in by ear, and `160` gets a line saying the default assumes 256 frames.
- **The workgroup acceptance test** (`170` § Audio workgroup). It's the other half of #9 and not about devices. The overrun journal shows the "`instruments` high, every track small" shape with all 11 workers in the workgroup, about 1,500 times since 2026-10-14. That moves to an issue of its own.

## Things to check while building

- **Getting the mixer back.** Checked (coreaudio-rs 0.14): `AudioUnit`'s drop stops the unit (`AudioOutputUnitStop`, which waits for the render callback), uninitializes it and only then frees the callback, so the hand-back can't race a running callback. The callback owns the mixer through `Returning`, whose drop sends it home on a one-slot channel; a stream that fails to build drops it the same way. The last `Arc` of the stream can be dropped on cpal's disconnect thread while it delivers an error, so the engine waits on the channel (5 s) rather than assuming the hand-back is synchronous.
- **Following the default device.** Picked the second: the engine always opens the *specific* device (`default_output_device()`'s ID looked up again with `device_by_id`, which cpal opens as a plain HAL unit), so cpal reports the rate changing (`StreamInvalidated`) and the device going away (`DeviceNotAvailable`). The default is followed by a poll on the engine thread, every 500 ms, instead of a `kAudioHardwarePropertyDefaultOutputDevice` listener: no unsafe, the same on every platform, and the same poll opens a device when none was. Hand test with headphones in and out still owed.
- **Outside buffer changes** aren't reported by cpal in either mode. A listener on `kAudioDevicePropertyBufferFrameSize` would catch them. Check whether they happen in practice (another app on the same interface setting its own buffer does) before adding it.
- **Reactivating in place.** Built UI-driven, not as decision 8's steps 1–2 had it: every frame the UI compares each plugin's activation rate (`TrackInstrument::sample_rate`) with the stream's (`StreamStatus`; every restart wakes the UI once the rate is published, footer line or not), and asks for a stale voice back with `PluginHostCommand::Return`. The mixer stops it on the audio thread (`InstrumentVoice::stop_processing`: CLAP's `stop_processing` belongs there) and sends it the way a removed voice goes; the `"plugin-host"` thread passes it to the UI (`VoiceBack::Returned`) instead of dropping it. The UI reactivates it (`InstrumentEditor::reactivate`) and inserts it at the new rate, note reset and gain ramping in. One path covers a voice inserted at a stale rate too (a project staged across the change, a pick in the restart's window), and a plugin removed while its voice is out finishes its teardown when the voice arrives (`pending_instance_drop`, first in first out per slot). Each voice goes back as soon as it is ready, not all at once; holding them for one insert, under the restore panel, is phase 3.
- **CLAP reactivation with clack 0.2.** Built: `ClapVoice::reactivated` takes the voice apart, gives its stopped processor back to `PluginInstance::deactivate`, activates again at the new rate and keeps the buffers and the parameter bridge (its report clock set to the new rate).
- **VST3 reactivation.** Built: `component::reactivate` runs `setProcessing(false)` → `setActive(false)` → `setupProcessing` → `setActive(true)` → `setProcessing(true)` on the main thread, and the voice takes the new rate and the tail `getTailSamples` now gives. Some plugins misbehave on a second `setupProcessing`: try the plugins on the test machine (Serum 2, Omnisphere, the NI ones that took the GL context). A plugin that refuses, in either format, is loaded again from its live state (`Display::reload_slot_instrument`, its editor reopened if it was open); one that won't load either leaves its track silent, and the footer says so.
- **Device list changes.** The tab needs the list current while it's open, and the missing device needs noticing when it comes back (decision 4). A `kAudioHardwarePropertyDevices` listener, or a slow poll on a watcher thread like `midi_watcher`, sending `UiEvent::AudioDevicesRefreshed`. A poll is simpler and matches MIDI.
- **The click and MIDI Out alignment** after a buffer change: `SCHEDULE_DELAY_FRAMES` moves, so the click's lead over the plugins stays one buffer, and the MIDI-Out offset is now off by the difference (§ Not in this design). Noted in `160` § the default.
- **The overrun journal across restarts.** Done: each record carries the mixer's session count, bumped on every restart, and the journal writes a header whenever it changes. One `"audio-journal"` thread for the app.

## Phases

1. **The engine restarts.** The engine thread's command loop, the mixer surviving its stream, the pool rebuilt each time, a runtime schedule delay, outside changes as triggers, the footer line and the journal session. No tab yet: this alone fixes #9 and the stale rate and buffer, with plugins handled by the simplest working path (reactivate if the format allows it, else hold their tracks silent until relaunch, and say so in the footer).
   - *Built:* all of it. Each voice carries the rate it was activated at (`PluginHostCommand::Insert::sample_rate`); the mixer holds a voice at another rate silent until the UI has reactivated it in place (§ Things to check: Reactivating in place), with a reload from its state as the fallback. With no device at launch the app starts silent and the engine opens one when it appears, rather than staying silent for the session. Hand tests on the plugin list owed.
2. **The Audio tab.** Device, rate and buffer choices, the readout, the three settings fields and the missing-device switch-back.
3. **Plugin reactivation polish.** One voice per frame under the restore panel, the reload fallback per plugin, and the hand tests on the plugin list.

## Settled when agreeing (2026-11-13)

1. **Plugins are reactivated in place** on a rate change (decision 8), with a reload from saved state kept only as a per-plugin fallback.
2. **The transport keeps running** through a restart (decision 9).
3. **The Audio tab comes first** and is the tab Settings opens on (decision 1).
4. **One sample rate for the app**, not one per device (decision 3).
