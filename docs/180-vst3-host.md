# VST3 Instrument Host (macOS)

`src/core/plugin_host/vst3/` is the VST3 format module of the plugin host —
one of the two formats behind the shared `InstrumentVoice` / `InstrumentEditor`
traits described in `130-plugin-host.md`. **Read `130` first**: everything about
the audio path, the event scheduler, the mixer, the transport snapshot, the
editor window and the key guard is shared and lives there. This file is only
about what VST3 does differently.

**VST3 and newer only — never VST2.** The VST2 SDK is no longer licensed by Steinberg and can't be redistributed under the project's `MIT OR Apache-2.0`, so no VST2 (or older) hosting, headers or bindings enter the repo. Using the "VST" name follows Steinberg's trademark guidelines; see `240-release-plan.md` § C.

## Status

**VST3 hosting is feature-complete.** A track can be routed to a VST3 instrument
from the browser panel's Plugins category, it makes sound, responds to the mod wheel and pitch bend,
`v` opens its editor, and its preset survives a project save.

Note that a VST3 track survives save/load without the `format` field the plan
reserved for it: `sync_instruments_to_tracks` matches a persisted
`InstrumentRef` against the catalog by bundle path plus class id, and a `.vst3`
path can only match a VST3 entry. The field is still worth adding for
robustness, but it is not what makes the round-trip work.

## Why raw bindings, and why not `vst3-host`

The host is written directly against the [`vst3`](https://crates.io/crates/vst3)
crate (Micah Johnston, MIT/Apache-2.0) — raw bindings generated from Steinberg's
C++ headers. That crate sits where `clap-sys` sits, **not** where `clack-host`
sits: it wraps COM pointer handling and nothing else, so the safe host layer
CLAP gets for free from `clack-host` is ours to write.

Considered and rejected:

- **`vst3-sys`** (RustAudio) — GPLv3. Would infect the whole app. It is also
  less complete and effectively superseded by `vst3`.
- **`vst3-host`** (MIT, ~0.9) — the closest thing to a `clack-host` equivalent,
  and genuinely useful as a *reference implementation* for the COM plumbing.
  Rejected as a dependency because it ships its own `cpal` stream and `midir`
  and states its audio path is "correctness-first, not lock-free/real-time
  tuned" — precisely the part `core::audio` already does carefully. Using it
  would mean fighting it for control of the realtime path.

**Licensing is not a constraint any more.** Steinberg relicensed the VST3 SDK to
**MIT** in late 2025 (SDK 3.8). No signed agreement, no GPL, no source-disclosure
obligation. Most advice online predates this and should be disregarded.

## Module layout

| File | Responsibility |
|---|---|
| `mod.rs` | `scan_catalog()` — VST3's single entry point into the shared catalog. |
| `module.rs` | `Vst3Module` (a loaded bundle + its `IPluginFactory`), `with_module` (load, use, unload), `load_module`. Only ever runs in a scan child. |
| `child.rs` | The out-of-process scanner: `SCAN_FLAG`, `run_if_scan_child` (the child half, called first thing in `main`), `scan_bundle_out_of_process` (the parent half), the per-bundle timeout. |
| `cache.rs` | `ScanCache` — the on-disk, mtime-keyed record of what each bundle yielded, so a cold scan happens once per plugin install rather than once per launch. |
| `discovery.rs` | `scan_catalog()` (cache-or-spawn per bundle), `classes_in_bundle` (the child's actual work), the three-tier `getClassInfo*` fallback, the instrument filter, `uid_to_hex` / `uid_from_hex`. |
| `component.rs` | `load()` — the COM lifecycle from a class id to an activated plugin — plus `LoadedPlugin`/`MainThreadHalf`, controller acquisition, connection-point wiring, and bus configuration. |
| `host.rs` | The COM objects handed *to* a plugin: `Vst3Host` (`IHostApplication`, including `createInstance`), `ComponentHandler`, `HostEventList` (`IEventList`). |
| `params.rs` | The UI→processor parameter bridge: `ParamSender`/`ParamReceiver` over an `rtrb` ring, and the `IParameterChanges` / `IParamValueQueue` the plugin reads. |
| `message.rs` | `HostMessage` (`IMessage`) and `HostAttributeList` (`IAttributeList`) — the objects a plugin asks the host to create so its two halves can exchange messages. |
| `voice.rs` | `Vst3Voice` — `impl InstrumentVoice`; the `ProcessData` assembly over the shared `PortBuffers` (`../buffers.rs`), the `ProcessContext` translation (tempo, positions in quarter notes, the cycle, and the project's meter as `timeSigNumerator`/`Denominator` with `barPositionMusic` from `BlockTransport`), and the observed-silence idle rule. |
| `editor.rs` | `Vst3Editor` — `impl InstrumentEditor`; the `IPlugView` lifecycle, `HostPlugFrame` (the `IPlugFrame` a view resizes itself through), the `!Send` half's ownership and teardown ordering. |
| `events.rs` | MIDI → VST3: `translate` (the three-way split), `NoteIds` (note-on ↔ note-off id pairing), `MidiMap` (the `IMidiMapping` table). |
| `state.rs` | Preset capture and restore: the two-half container format, `capture`, `apply`. |
| `stream.rs` | `MemoryStream` — the in-memory `IBStream` a plugin reads and writes its state through. |

## Loading a bundle

A `.vst3` bundle's executable exports three C symbols: `bundleEntry` /
`bundleExit` (module lifecycle) and `GetPluginFactory` (the class factory).

The bundle is loaded through **`CFBundle`**, not a bare `dlopen`, because
`bundleEntry` is *handed the `CFBundleRef`* — that is how a plugin locates its
own resources, and JUCE-based plugins in particular need it. Passing null there
leaves them unable to find their UI assets. `objc2-core-foundation` supplies
`CFBundle`; it was already in the tree via `objc2-foundation`, so it pulls no
new crates.

`GetPluginFactory` returns a pointer the plugin has already `addRef`'d, so it is
adopted directly into a `ComPtr` rather than retained again.

### The scan unloads every module, and that is load-bearing

`with_module` loads a bundle, runs one closure against it, then calls
`bundleExit` and unloads. **This is not tidiness — it is what keeps app exit
alive.**

`bundleEntry` is not a cheap metadata read. A big plugin starts its real runtime
there: Kontakt 7/8, Komplete Kontrol, Massive X and Reaktor 6 were each observed
spinning up `boost::asio` and `spdlog` worker thread pools during a scan. Leave
~60 of those loaded for the process lifetime and `exit()` eventually runs all of
their static destructors, underneath their own still-running threads, in an
order nobody has ever tested. Observed result, with every scanned module held
loaded:

```
exit → __cxa_finalize_ranges → std::terminate → _objc_terminate → abort
```

— a C++ exception escaping a static destructor at process teardown. Calling
`bundleExit` as soon as the metadata is read lets each module tear itself down
while its runtime is still healthy. It also made the scan **3.3× faster**
(92 s → 27 s), since the process is no longer accumulating sixty plugin runtimes.

This is sound only because the scan instantiates nothing — the module is the
caller's alone and no plugin object outlives the closure. **Instantiation will
need the opposite policy**: a module a plugin instance came from must stay
loaded for as long as that instance lives, mirroring `clap::discovery`'s
`ENTRY_CACHE`. That is a different lifetime and gets its own entry point when
loading lands.

## Enumerating classes

A factory lists *classes*, not plugins, and every kind of class is in the same
list — audio effects, instruments, and the separate edit-controller classes that
pair with them. Two filters narrow it to what the plugin catalog should offer:

1. `PClassInfo::category` must be `"Audio Module Class"` (`kVstAudioEffectClass`
   in the headers — a `#define` of a plain string literal, so the generated
   bindings don't carry it and we declare it ourselves).
2. `PClassInfo2::subCategories` must contain the token `Instrument`. It is a
   `|`-separated list, matched **by token, not substring**, so `Instrument|Synth`
   matches and a hypothetical `NotAnInstrument` does not.

Class info is read through the richest call the factory supports, in order:
`IPluginFactory3::getClassInfoUnicode` (proper UTF-16 names) →
`IPluginFactory2::getClassInfo2` → `IPluginFactory::getClassInfo`. The tiers are
**not** interchangeable: only `IPluginFactory2` and up report sub-categories, so
a factory offering only the bare `IPluginFactory` yields classes with no
sub-categories, which filter 2 then rejects. That is the correct outcome —
offering every audio module and hoping it's an instrument would be worse than
offering none. Such factories are vanishingly rare in practice.

## Class UIDs

VST3 identifies a class by a 16-byte `TUID`, where CLAP uses a string id.
`PluginCatalogEntry::plugin_id` carries it as **32 lowercase hex characters**
(`uid_to_hex`), so the shared catalog and the persisted `InstrumentRef` need no
VST3-specific field.

This is a plain hex dump of the raw bytes, deliberately **not** Steinberg's
canonical `FUID` registry string: nothing outside Stev reads it, so all it
must do is decode back to the same 16 bytes and stay stable across runs. A test
pins the exact encoding — including the sign boundary, since `TUID` is
`[c_char; 16]` and so signed on this platform — because the decoder that
instantiates a class by id has to agree with it.

## Catalog delivery is incremental

`catalog::scan_catalog` takes a callback and fires it **once per format**, each
time with the whole catalog so far, already sorted. `start_plugin_catalog_scan`
forwards each to `Display`, which replaces what it holds; the sender is dropped
when the scan finishes, so a **disconnected channel is what marks the scan
complete** (and `plugin_catalog_rx.is_some()` therefore means "still scanning").

The reason is the cost asymmetry: a CLAP scan is close to instant, a VST3 scan of
~60 bundles takes ~27 s. Waiting for the slow one would leave the browser's
Plugins category empty of *every* plugin — CLAP included — for that whole time, which would have
been a regression against the CLAP-only behaviour. Formats are scanned cheapest
first. While the scan is still running the Plugins category ends in a dim
`Scanning…` row, so a plugin that simply hasn't been found *yet* doesn't read
as one that isn't installed.

## Scanning runs out of process, and it has to

**A VST3 bundle cannot be scanned inside Stev at all.** Two separate
failures established this, in order:

1. **Spawning the scan before `NSApplication` exists hangs the app on launch** —
   no window, no crash, the log simply stopping after the virtual MIDI ports.
   Plugin `bundleEntry` code touches AppKit from whatever thread calls it, and
   doing that while winit is concurrently creating `NSApplication` on the main
   thread wedges launch.
2. **Some plugins' `bundleEntry` requires the main thread outright**, which no
   amount of ordering can provide. Kontakt 8's constructs a full Qt
   `QApplication`, whose Cocoa platform integration calls HIToolbox's Text
   Services Manager, which asserts it is on the main queue and traps:

   ```text
   scan → ModuleStartup::startup() → ni::qt::Module::Module
        → QApplicationPrivate::init → QCocoaIntegration → QCocoaInputContext
        → TSMGetInputSourcePropertyWithSetter
        → islGetInputSourceListWithAdditions → dispatch_assert_queue → SIGILL
   ```

Our own main thread is not available to give: it belongs to winit/egui, a cold
scan takes seconds to a minute, and handing it to a second GUI toolkit's event
dispatcher is exactly the conflict every serious host scans out-of-process to
avoid. A CLAP `init()` is a light entry-point registration and provokes none of
this, which is why the CLAP scan has always been a plain background thread.

### The design

The parent re-invokes **its own binary** with `--scan-vst3-bundle <bundle>
<out.json>`, **one child per bundle**. The child scans that bundle on *its* main
thread — which is free — writes the class list as JSON, and exits.

- `run_if_scan_child` is the **first statement in `main`**, before any app
  setup. A scan child must not open an audio device, MIDI ports or a window.
- **One child per bundle**, not one for the batch, so a plugin that crashes or
  hangs costs exactly itself. The parent records the failure and the rest of the
  catalog is unaffected.
- **Results go to a file, not stdout.** Plugins print to stdout and stderr as
  they initialise (Repro-1 also writes a log to the Desktop) and would corrupt a
  piped payload. The child's own stdio is sent to `/dev/null`.
- **The child ends with `libc::_exit`**, never a normal return. A normal exit
  runs the loaded plugin's static destructors underneath its own still-running
  worker threads, which is its own reliable way to abort (see the `bundleExit`
  section above — same failure, different trigger). The result file is written
  and closed first.
- A child that outlives `SCAN_TIMEOUT` (30 s) is killed and recorded as failed.
  Generous on purpose: the heaviest sampler libraries take seconds to
  initialise, and a false timeout costs the user that plugin.

### The cache

`ScanCache` (`vst3-catalog-cache.json` in the platform cache directory,
`paths::cache_dir()`, e.g. `~/Library/Caches/stev/`) records what each bundle yielded, keyed by the bundle
directory's **modification time**. An unchanged bundle is never rescanned;
updating or reinstalling one invalidates just that entry. Measured on a
20-bundle collection: **cold 3.5 s, warm 60 ms.**

Three things are deliberately cached that might look like they need not be:

- **Effect-only bundles**, as an entry with no classes. Most installed bundles
  are effects, so not remembering them would leave most of the scan cost in
  place.
- **Failed scans**, so a plugin that crashes or hangs its child is skipped next
  launch rather than costing its full timeout every single start. Changing the
  plugin clears the flag.
- **Nothing about uninstalled bundles** — `retain_installed` drops them, so the
  file does not grow forever.

`CACHE_VERSION` guards the format; a file from another version is discarded
rather than misread, which is always safe because the scan is reproducible.

### Testing limitation

The parent half cannot be exercised from a unit test: `scan_bundle_out_of_process`
spawns `std::env::current_exe()`, which under `cargo test` is the **test
binary**, whose `main` is libtest's and never calls `run_if_scan_child`. The
child half, the cache and every pure helper are unit-tested; the loop that joins
them is covered by running the app.

## Loading a plugin

`component::load` runs on the **eframe main thread** — required, since some
plugins' module initialisation demands it, and since the `!Send` half must be
created where it will live. The sequence is fixed by the spec and every step
matters:

1. `createInstance(cid, IComponent)` from the cached module's factory.
2. `component.initialize(IHostApplication)`.
3. Query `IAudioProcessor` off the component.
4. Get the edit controller: the same object for a single-component plugin,
   otherwise `getControllerClassId` + a second `createInstance` + its own
   `initialize`.
5. `IConnectionPoint::connect` **both ways**, then `setComponentHandler`.
6. Configure buses (below), `setupProcessing`, `setActive(true)`,
   `setProcessing(true)`.

### The Send split

CLAP gets this for free: `clack-host` hands back a `!Send` `PluginInstance` and
a `Send` `PluginAudioProcessor`. VST3 has no such division, so `load` returns
`LoadedPlugin { main: MainThreadHalf, processor, io }` and Stev maintains it.

**The compiler will not catch a mistake here.** `ComPtr<I>` is unconditionally
`Send` in these bindings, so nothing stops an `IComponent` being moved to the
audio thread. The split is upheld by *construction* — only `processor` ever
leaves the main thread — and `MainThreadHalf` carries a
`PhantomData<*const ()>` so at least the main-thread half cannot be moved by
accident. This is the largest hand-asserted invariant in the module.

### Bus configuration

Three things in `configure_buses` are load-bearing:

- **Every audio bus is activated.** A bus the host never activates may be left
  unallocated by the plugin, and `process` then writes through a null channel
  pointer — the same crash commit `bbb6d06` fixed for CLAP aux output ports,
  reached from the other direction.
- **Every event input bus is activated**, or an instrument receives no notes at
  all and is silent with no error anywhere to explain it.
- **The layout is read back after negotiation, not assumed.**
  `setBusArrangements` is a request a plugin may decline, so buffers are sized
  from what `getBusInfo` reports afterwards.

The negotiated layout must then fit the fixed per-block arrays `render_block`
fills (`MAX_BUSES` = 64 buses and `MAX_CHANNELS` = 256 channels per direction,
in `voice.rs`); `check_bus_capacity` rejects a larger one at load with an error,
rather than the audio thread truncating buses the plugin expects.

## Rendering a block

`Vst3Voice::render_block` assembles one `ProcessData`. Two details differ from
the CLAP path:

- **Channel-pointer arrays are rebuilt every block** rather than cached, in
  fixed-size arrays on the stack (`fill_buses` packs every bus's channel
  pointers into one flat array, then points each `AudioBusBuffers` at its
  slice). A cached `*mut *mut f32` would silently rot if a buffer ever moved,
  and raw pointers stored on the voice would make it `!Send` — needing another
  hand-asserted `unsafe impl Send`. They used to be `collect()`ed into fresh
  `Vec`s, which allocated and freed on the audio thread every block.
- **There is no `ProcessStatus::Sleep`.** Nothing in VST3 says "I am done until
  you send me something", so idling has to be inferred — and **getting it wrong
  loses audio**, not just a block of CPU. A sleeping voice is not processed at
  all, so anything the plugin would have produced on its own (a feedback delay
  whose taps are separated by digital silence) simply never happens.

  Two things therefore decide it together, in `may_sleep`:

  - **`getTailSamples`**, the plugin's own answer to "how much longer might I
    still produce output". `kInfiniteTail` (`u32::MAX`) means *never sleep this
    voice*.
  - **`MIN_SILENT_SAMPLES`** (10 240, ~0.2 s at 48 kHz), a floor that stops a
    voice thrashing in and out of sleep across the gaps in a busy part. It is
    explicitly not a tail estimate.

  Silence is tested as exactly `0.0`, so an asymptotically decaying tail keeps
  the voice awake by itself — the failure direction is staying awake too long,
  which is safe.

  Measured across the installed collection: 14 of 16 plugins declare **no** tail
  and so sleep on the floor alone, while **Kontakt 8 and Reaktor 6 declare an
  infinite tail and never sleep**. That is the plugin's own instruction — both
  are hosts-within-hosts that can begin producing output at any time — but it
  does mean those two cost a `process()` call per block even while silent. If
  that ever matters on the DSP meter, capping an infinite tail at some generous
  bound (tens of seconds of continuous digital silence) is the knob to reach
  for, and would be a one-line change in `may_sleep`.

`HostEventList` holds its events in an `UnsafeCell` and is `unsafe impl Sync`.
The invariant is ownership, not synchronisation: one list belongs to one voice,
every access is on the audio thread inside one `render_into`, and the VST3 spec
scopes `ProcessData` to the `process` call so the plugin cannot retain it. The
safety comment on the impl states this in full.

## The editor

A VST3 editor is an `IPlugView` the **edit controller** creates on demand. The
window it lives in is the same
[`PluginWindow`](crate::core::plugin_host::window) the CLAP host embeds into, so
both formats share the window plumbing, the remembered position, and the
app-wide `Space`/`v`/`.`/`0` key guard — `PluginWindow::new` registers itself,
so that came for free.

The open sequence, in the order the spec requires:

1. `controller.createView(kEditor)`.
2. `isPlatformTypeSupported("NSView")` — a plugin that says no gets no editor
   rather than a blank window.
3. `getSize` for the initial window size.
4. `PluginWindow::new` at that size, at the remembered position.
5. **`setFrame` before `attached`**, so a view that wants to resize itself
   during attach has somewhere to say so.
6. `attached(content_view, "NSView")`.
7. `getSize` **again** — some plugins only report a meaningful size once
   attached — and resize the window to match.

Closing runs it backwards: `removed()`, then `setFrame(null)`, then drop the
window. **Closing destroys the view; it is never merely hidden.** A
hidden-but-alive editor keeps its animation timers running and measurably raises
idle CPU — the lesson from the CLAP host, and it applies identically here.

### Differences from the CLAP editor

- **There is no floating option.** A CLAP plugin may own its own OS window and
  many do; a VST3 view is always parented into one of ours. That makes this the
  simpler of the two paths — no `create_floating` branch, no plugin-initiated
  window close to detect.
- **Resize requests arrive synchronously on this thread.** CLAP's
  `request_resize` can be called from anywhere (including the audio thread) and
  is stashed in atomics. `IPlugFrame::resizeView` is specified as a UI-thread
  call, so it only has to cross from the COM object to the editor — a plain
  `Rc<Cell<...>>`.

  It is still *recorded* rather than applied on the spot. The plugin calls
  `resizeView` from inside its own code, so re-entering the editor — which owns
  the window the frame would have to touch — at that exact moment is the kind of
  aliasing worth not having. `pump` drains it on the next frame, resizes the
  window, then calls `onSize` to acknowledge, which is the order `IPlugFrame`
  specifies.
- **`pump` returns `None`, always.** CLAP has a host timer extension whose
  callbacks the host must drive, and the returned interval is what schedules
  them. VST3 has no equivalent — the view animates off the AppKit run loop — so
  there is nothing to schedule.

`pump` also reconciles visibility (closing our window with its title-bar button
never reaches the plugin, so a no-longer-visible window means "closed", and
closing destroys) and keeps the editor floating above the main window while
Stev is active and no project dialog is open (`editor_level`), both
exactly as the CLAP editor does.

## Single- vs dual-component plugins

**This distinction decides how much work the host has to do, and getting it
wrong produces a bug that looks like anything but a host bug.**

A VST3 plugin comes in one of two shapes:

- **Single-component** — one object implements both `IComponent` and
  `IEditController`. Moving a control in the UI mutates the very state `process`
  reads, so the sound follows with no host involvement at all. Repro-1 is one.
- **Dual-component** — the processor and the controller are *separate objects*
  that share nothing. Everything that has to cross between them goes through the
  host. Spire and Serum 2 are both like this.

**You cannot tell which a plugin is from the outside**, and guessing is a
mistake this file made once already — Serum 2 was assumed single-component
purely because it made sound. The only reliable test is whether
`component.cast::<IEditController>()` succeeds (single) or
`getControllerClassId` is needed (dual), which is what `obtain_controller` does.

For a dual-component plugin the host owes it two channels, and Stev
initially had neither:

1. **Parameter changes**, via `IComponentHandler::performEdit` → the host queues
   them → they arrive as `ProcessData::inputParameterChanges` in the next block.
   This is [`params`](super::params).
2. **Messages**, via `IConnectionPoint` — but a plugin *cannot allocate an
   `IMessage` itself*. It asks the host through
   `IHostApplication::createInstance`, and a host that declines silently removes
   the channel. This is [`message`](super::message).

The symptom of missing these is precise and thoroughly misleading: the plugin
loads, the editor opens, the preset browser works, every control in the UI
updates and shows the right values — **and the sound never changes**, because
only the controller ever heard about any of it. Nothing errors. Nothing logs.

**The two channels are independent, and a plugin may need either.** That is not
a theoretical point — the two plugins that forced this work each needed a
different one:

| Plugin | `createInstance` calls at load | What was broken | Fixed by |
|---|---|---|---|
| **Spire** | 0 | Loaded and played, editor and preset browser both worked, but the sound never left the init patch | `params` — the `performEdit` bridge |
| **Serum 2** | 3 | Would not load at all | `message` — host-created `IMessage`/`IAttributeList` |

Implementing only the one that explained the symptom in hand would have left the
other plugin broken, with no hint as to why. Both are host obligations the spec
states plainly; neither is optional.

Worth knowing when the next plugin misbehaves: a dual-component plugin that
won't load is probably being refused something by `createInstance`, and one
whose UI works but whose sound is stuck is probably missing the parameter
relay. Neither reports an error.

### The parameter bridge

`performEdit` is a UI-thread call and `process` is on the audio thread, so the
crossing is an `rtrb` ring — the same reasoning as every other audio-thread feed
here, since a `crossbeam_channel` may allocate inside `pop`.

- The producer is behind a `Mutex`. Not for the audio thread's sake — it never
  touches it — but because `performEdit` is *specified* as UI-thread and
  plugins have been known to report from their own workers, where a `RefCell`
  would panic and a mutex merely serialises.
- Every change lands at **sample offset 0**. `performEdit` carries no timing
  information; it is a gesture, not an automation curve.
- Repeated changes to one parameter within a block **collapse to the latest**. A
  knob drag emits many; the plugin should see one queue holding where it ended
  up.
- The queue pool is pre-allocated (`MAX_PARAMS_PER_BLOCK` = 512), so a preset
  load that touches hundreds of parameters never allocates on the audio thread.
  When a block is full, `drain_ui` stops (it peeks before it pops) and the rest
  stay in the ring for the following blocks, in order — a preset loaded from
  the plugin's own UI can send thousands of `performEdit`s at once. Stopping
  also caps what a burst of distinct parameters costs one block in the linear
  dedupe scan (repeats to parameters already in the block still drain). Only a MIDI CC that finds the
  block full is dropped; it has no ring to wait in.
- A sleeping voice is woken while the ring is non-empty
  (`Vst3Voice::wake_on_request`, via `ParamReceiver::ui_pending`). Without that,
  a knob turned while the voice slept — or the tail of a carried-over burst —
  would reach the processor only with the next note, and a ⌘S in between
  would save the processor's stale state.

## State persistence

VST3 splits state in two and a host is expected to keep **both**:

- `IComponent::getState` — the processor's state. This is the sound.
- `IEditController::getState` — whatever the UI wants to remember that is not a
  parameter (the open page, a zoom level, a browser filter).

**…but only when the controller is a separate object.** On a single-component
plugin `IEditController::getState` *is* `IComponent::getState` on the very same
object, so treating the two halves independently stores the plugin's state twice
and, on restore, applies it three times (`setState`, `setComponentState`,
`setState` again) — none of which the spec asks for. Fixing it halved those blobs
(Repro-1 12807 → 6410 bytes).

`controller_has_own_state` is the single rule both `capture` and `apply` consult,
deliberately one function rather than a condition written twice — they must
agree, and the failure when they don't looks nothing like a state bug.

Both halves go into one blob so the rest of Stev keeps seeing a plugin's
state as a single opaque `Vec<u8>`, the same shape CLAP's blob has, in the same
base64 field of the `.stev`. The container is `MSV3` + a version byte + two
length-prefixed halves; anything that is not a well-formed blob of a known
version is **refused**, and the plugin loads at its defaults. A wrong preset is
worse than no preset.

**Restore order is fixed by the spec**, and `apply` follows it exactly:

1. `component.setState(stream)` — before activation.
2. **`component_stream.rewind()`**, then
   `controller.setComponentState(stream)` — *the same bytes*, so the UI ends up
   showing what the processor is actually doing. The rewind is not optional: the
   plugin's own `setState` leaves the cursor at the end, and a controller handed
   an exhausted stream silently restores nothing and reports no error.
3. `controller.setState(controller_stream)` — its own state, if any.

### Teardown order, and why it is not cosmetic

`Vst3Editor::terminate` runs strictly backwards from `load`:

1. `setProcessing(false)`
2. `component.setActive(false)`
3. **disconnect both connection points**
4. `controller.terminate()`
5. `component.terminate()`

An earlier version terminated the controller *first*, while the component was
still active and the two were still connected, and never disconnected at all.
That reproducibly segfaulted u-he's Repro-1 and Repro-5 — they are entitled to
assume a host tears down in the reverse of the order it built up. It went
unnoticed through two phases because the probes of the time never terminated a
plugin at all.

## MIDI, and the three ways VST3 splits one stream

CLAP takes MIDI 1.0 bytes verbatim. VST3 has no general MIDI input at all, and
one incoming stream fans out three ways:

| MIDI | Becomes |
|---|---|
| note on / note off | a typed `Event`, carrying a `noteId` |
| poly key pressure | a typed `Event`, carrying the *sounding* note's id |
| CC, channel pressure, pitch bend | a **parameter change** |
| anything else (program change, …) | dropped |

The third row is the awkward one and the reason this took its own phase. A VST3
plugin does not receive controllers. It exposes *parameters*, and tells the host
through `IMidiMapping` which parameter each controller ought to drive. So a mod
wheel does nothing at all unless the host asks that question and rewrites every
CC into automation on the answer.

`MidiMap` asks it once at load — 16 channels × 130 controllers, ~2000 vtable
calls — so the audio thread is left with an array index. The 130 is MIDI's 128
controllers plus VST3's two synthetic ones, `kAfterTouch` and `kPitchBend`,
which exist precisely because those messages have no CC number.

Changes go into the same `params::HostParameterChanges` the UI relay fills.
That is why `ParamReceiver::reset` is called from `clear_events` at the **end**
of a block rather than at the start of `render_block`: CCs are pushed in during
the mixer's event dispatch, which runs first, and resetting later would throw
them away.

### `kResultOk` does not mean "mapped"

A plugin may report success from `getMidiControllerAssignment` and still write
**`kNoParamId`** (`0xFFFFFFFF`), meaning "this controller drives nothing".
Cthulhu does exactly that, for all 130. Taking the sentinel at face value routes
every CC to parameter `0xFFFFFFFF`. `assigned_param` is the one-line filter that
catches it, and it exists as a named function so it can be tested.

Measured across the installed collection: every plugin answers all 130 queries,
but Cthulhu's are all the sentinel (0 real mappings) and Spire maps only 12.
The rest map the full set.

## Known caveats

- **A cold scan still blocks VST3 plugins from appearing** for its duration —
  seconds on a small collection, up to a minute on a large one — and a project
  loaded before it lands (whose `sync_instruments_to_tracks` looks the plugin up
  in the catalog) will not find its plugin. The cache means this is a
  once-per-install cost rather than a once-per-launch one, and the catalog is
  delivered per format so CLAP plugins never wait for it. Not fixed: the
  project-load race.
- **`moduleinfo.json`** — the VST3 3.7.5+ manifest that exists precisely so
  hosts need not load the binary — would make a cold scan nearly free, but only
  9 of the 62 bundles measured here shipped one. Worth revisiting as a fast path
  *before* falling back to a child process, not as a replacement for it.
- **Sorting is by name across formats**, not grouped by format, so a plugin
  installed as both CLAP and VST3 appears as two adjacent rows distinguished
  only by the format tag. Deliberate — the user is looking for a name.
- **`bundleExit` is trusted to be safe to call.** It is by spec (the module
  refcounts the entry/exit pair) and every host does it, but a
  badly-behaved plugin could in principle misbehave here. A bundle that exports
  no `bundleExit` is simply left loaded.
- **A plugin that refuses to instantiate, or that kills the process, takes its
  track with it.** Measured across the installed collection: 15 of 17 classes
  instantiated and 11 produced audio from a note-on; the silent ones (Kontakt,
  Reaktor, DecentSampler) are correctly silent with no library loaded, and
  Cthulhu is a MIDI effect. Omnisphere's `createInstance` fails deterministically
  — handled as a load error, not a crash, most likely wanting authorization or a
  fuller host environment. The VST3 build of OsTIrus calls **`exit(0)`**
  somewhere in its own load path — a deliberate exit, not a crash, reproducible
  and at no consistent point. Nothing an in-process host can defend against, and
  its CLAP build is unaffected. Out-of-process *hosting* (not just scanning) is the only real
  answer, and is not planned.
- **Pitch bend is 14-bit and centre must land exactly on 0.5** — a pitch wheel
  that rests slightly sharp is immediately audible. The LSB-then-MSB assembly is
  pinned by a test, because getting it backwards still yields plausible numbers.
- **Note-offs must carry their note-on's `noteId`.** Handled by `NoteIds`, but
  it is the sort of invariant that is easy to break: a plugin that keys voices
  by id leaves the note sounding forever if they disagree. A note-on at velocity
  0 counts as a note-off, per the running-status convention.
- **A plugin with no edit controller gets no editor**, and one whose view
  declines `NSView` gets none either. Both are logged and leave the instrument
  playing; neither is an error the user sees. Measured across the installed
  collection, every plugin that has a controller supports `NSView`.
- **Three plugins report a degenerate `0x0` view size** before attach —
  Kontakt 8 (whose `getSize` fails outright), Omnisphere and Spire. They open at
  the 900×600 fallback and are resized by the post-attach `getSize`, which is
  why that second query is not optional.
- **Massive X's `createView` did not return** within 25 s in a bare test process
  with no `NSApplication`. Whether that reproduces inside the running app is
  unknown — the app does have a live `NSApp`, but `show()` is called
  synchronously from the UI thread with the run loop not pumping, so a view that
  needs the run loop to make progress would hang there. Nothing in the host can
  defend against a synchronous COM call that never returns. Worth checking
  first if an editor ever appears to freeze the app.
- **A plugin may restore its sound without restoring its browser UI, and that
  is not something a host can fix.** u-he's Repro-5 recalls the right patch and
  opens the right preset category, but its browser does not show the preset as
  *selected*. Verified identical in Ableton Live, so it is the plugin's own
  state handling, not Stev's. Worth remembering as a shape: a restore that is
  right in every audible respect and wrong in a cosmetic one is far more likely
  to be the plugin than the host — the cheap test is another host, or the same
  plugin in its other format.
- **A byte-identical state round-trip is not guaranteed, and not a bug.**
  Measured across the installed collection, 15 of 16 plugins report state and
  accept it back (OsTIrus is the exception — it exits the process, see below);
  9 return byte-identical bytes on a re-capture and 6 do not (Cthulhu,
  DecentSampler, Kontakt 8, Omnisphere, Spire, TAL BassLine 101).
  Plugin state routinely embeds timestamps, seeds, cached paths and UI state, so
  a differing re-capture says little either way. What the probe does establish is
  that `getState` produces data, `setState` accepts it, and the plugin survives
  both — not that the sound is identical. Only using it proves that.
- **`HostAttributeList::getBinary` hands back a borrowed pointer** into the
  stored `Vec`, valid only while that attribute list lives. That is the
  ownership the VST3 API specifies, but it means a plugin that stashes the
  pointer past the message's lifetime reads freed memory. Nothing can be done
  about it from this side.
