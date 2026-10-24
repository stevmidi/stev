# A Downloadable macOS App

**Status: plan agreed 2026-10-24; phase 1 (the bundle script) merged, phase 2 (the hand test) run 2026-10-24 with its results below and its two bundle-only Dock bugs fixed, phase 3 (the About tab and the app menu) built and hand-tested 2026-10-24, phases 4–6 not started.** Level 1 of the `280` candidate *A downloadable macOS app*: an unsigned universal `Stev.app` in a zip, attached to each `v*` release, with the items that come with it (the hand test, the About tab with its update check, opening projects from Finder, a log file). Levels 2 (signed + notarized) and 3 (a Homebrew cask) stay in `280`. Read `280` § A downloadable macOS app first, and `080` § Releasing and § Licensing.

## Decisions

Settled before this doc (2026-10-24, in `280`):

1. **A universal binary** (`x86_64` + `aarch64`, joined with `lipo`), even though only Intel can be tested. The README and release notes say Apple Silicon is untested until someone confirms it runs.
2. **A manual "Check for updates"**: `/usr/bin/curl` against GitHub's `/releases/latest`, on a background thread, only when the button is pressed. No HTTP crate, no setting, no throttle, no self-updater.

Settled with this doc (2026-10-24):

3. **About is a settings tab**, `SettingsTab::About`, after Keyboard. Not a window and not a `ViewState`, which follows the settings-modal rule (overlays, not views).
4. **The log file lands before the release workflow**, so the first binary a stranger runs already writes one.

Proposed with this doc and approved 2026-10-24:

5. **Minimum macOS 11 (Big Sur)** for both slices: `MACOSX_DEPLOYMENT_TARGET=11.0` at build time and `LSMinimumSystemVersion` 11.0 in the plist. 11.0 is the floor for `aarch64` anyway, and one number is simpler to state than two. (Phase 1.)
6. **The `.stev` type is registered only together with the code that opens it** (phase 4), not in phase 1. Registered on its own, a double-click would launch Stev with nothing loaded: winit doesn't hand the app the open-documents Apple Event. That reads as a bug, and is worse than Finder's honest "no application" dialog. Phase 1's plist has no document types.
7. **The VST Compatible logo is pre-rendered, not rendered from SVG at runtime.** egui can't draw SVG without `egui_extras`' `svg` feature, which pulls in `resvg` and its tree. A script rasterises both SVG variants once, at 1× and 2×, to raw RGBA files beside them in `docs/images/vst/`, and `include_bytes!` + `ColorImage::from_rgba_unmultiplied` loads them. That adds no dependency, and the logo's `LICENSE.txt` already sits beside the files. (Phase 3.)
8. **The icon is checked in as `Stev.icns`**, made once by a script from `site/images/logo.svg`, so CI installs nothing to build a bundle. The robin goes on a macOS-style rounded tile, sized to Apple's icon grid, as a separate source SVG (`assets/macos/icon.svg`). The website's logo stays unchanged. (Phase 1.)

## Phases

Each phase is its own branch and leaves `main` shippable. Phases 1–2 change no app code unless the hand test finds a bundle-only bug.

### 1. The bundle, built locally

`scripts/macos/bundle.sh [--version x.y.z]` builds `target/macos/Stev.app` and `Stev-x.y.z-macos-universal.zip`:

- `rustup target add` both Apple targets (they are not added to `rust-toolchain.toml`, because the Linux and Windows CI jobs would download them for nothing), then `cargo build --release --target …` for each, with `MACOSX_DEPLOYMENT_TARGET` set, and `lipo -create` into one `stev`.
- The bundle layout:
  - `Contents/Info.plist`: generated from a template (`assets/macos/Info.plist.in`), with the version from `Cargo.toml`. It sets `CFBundleIdentifier` `com.stevmidi.stev`, `CFBundleName`/`CFBundleDisplayName` `Stev`, `CFBundleExecutable` `Stev` (phase 3), `CFBundleIconFile`, `NSHighResolutionCapable`, `LSMinimumSystemVersion`, `NSHumanReadableCopyright`, `LSApplicationCategoryType` `public.app-category.music`.
  - `Contents/MacOS/Stev`: the universal binary, capitalised since phase 3 so the app menu's items read "About Stev" (phase 2 found winit titles them with the executable's name).
  - `Contents/Resources/`: `Stev.icns`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-NOTICES.md`, the Plex `OFL.txt` (the font is compiled in, so its licence travels with the binary) as `IBMPlexSans-OFL.txt`, and the VST logo's `LICENSE.txt` as `VST-Compatible-Logo-LICENSE.txt` (renamed, so two licences don't sit side by side as `LICENSE.txt` and `OFL.txt` with nothing to say whose they are).
- **An ad-hoc signature over the whole bundle**: `codesign --force --sign - Stev.app`. This step is required, not cosmetic. Apple Silicon refuses to run an unsigned arm64 binary, and `lipo` plus the bundle edits can invalidate the linker's own signature. It doesn't make Gatekeeper trust the app (that is level 2), it just lets the app run at all.
- **Zipped with `ditto -c -k --keepParent`**, not `zip`, which loses the bundle's extended attributes and symlinks.
- `scripts/macos/make-icon.sh` builds `Stev.icns` from `assets/macos/icon.svg` with `resvg` (`cargo install resvg`; Homebrew's `librsvg` was tried and builds LLVM from source on an Intel Mac) and `iconutil`. It runs by hand when the icon changes, and its output is committed.
- Checks: `lipo -archs` names both architectures, `codesign --verify --deep --strict` passes, and `plutil -lint` passes on the plist.

### 2. The hand test, on the Intel Mac

Run once from the zip, unpacked from a browser download, so the quarantine flag is set as a user would get it. These paths have only ever run as a bare `cargo run`:

- **Gatekeeper**: the first launch is blocked, then System Settings → Privacy & Security → *Open Anyway* lets it run. (Since macOS 15, Control-click → Open no longer bypasses it.) Note the exact wording for the README.
- **Audio and timing**: CoreAudio output, the metronome, the clock timer and RT priority, the DSP meter, an audio export.
- **MIDI**: input ports, MIDI Out, the virtual port's name in another app.
- **Plugins**: a CLAP and a VST3 load, and their editors open. The out-of-process VST3 scan (`vst3/child.rs` re-runs `current_exe()`, now `Contents/MacOS/stev`) works, and the child doesn't flash a Dock icon.
- **Files**: settings and the library land where `core/paths.rs` says, independent of the working directory (`/` inside a `.app`). Finder drag of a `.mid`, save, load, and the browser.
- **Quit**: ⌘Q, Dock Quit and logout with unsaved changes.
- **The app menu**: the menu-bar name reads "Stev". Note whether the items read "About stev" / "Hide stev" / "Quit stev" (the process name), which phase 3 fixes.
- **Output that goes nowhere**: from a `.app`, stdout and stderr are discarded. List every error a user would need that is lost today. That list feeds phase 5.

Fixes for bundle-only bugs go on their own `fix/` branches, each with a regression test where the layer allows it.

**Results** (2026-10-24, macOS 15.6 on the Intel Mac, the 0.5.0 zip from `f16f883`, quarantined as a Safari download and moved to /Applications in Finder):

- **Gatekeeper**: blocked as expected. The dialog is "“Stev.app” Not Opened" (seen in Norwegian), with only *Move to Trash* and *Done*. System Settings → Privacy & Security → *Open Anyway*, then a second confirmation, lets it run, and later launches don't ask. The README takes the English wording and steps from Apple's support page ("Open a Mac app from an unknown developer"), not from a screenshot.
- **A Local Network prompt** ("find devices on your local network") appears the first time a project is opened. It comes from a plugin the project loads, not from Stev, which has no networking code or networking dependency. A bare `cargo run` never showed it because macOS asks on behalf of Terminal or Zed, which had already answered. Either answer leaves Stev working; denying may cost a plugin its controller or licence lookup. The README's Download section says so (phase 6).
- **The app menu**: the menu-bar name reads "Stev" (from `CFBundleName`), but the items read "About stev" and "Quit stev" (the executable's name). Confirmed, so phase 3 renames the executable to `Stev`.
- **Audio and timing, MIDI, files, quit**: all as in a bare `cargo run`. CoreAudio output, the metronome, the DSP meter and an audio export work; MIDI ports, MIDI Out and the virtual port work; settings and the library land in their usual places; a Finder `.mid` drag, save, load and the browser work; ⌘Q, Dock Quit and logout ask about unsaved changes.
- **Plugins**: a CLAP and a VST3 load and their editors open. **Bug**: during a full VST3 scan (cache deleted) a second Stev icon flashes in the Dock. Inside a bundle the scan child carries the bundle's identity, so a plugin that brings up AppKit while scanning gets it a regular Dock icon. Fixed on its own `fix/` branch, except a brief empty space the Dock opens and closes during a full scan, which is accepted (`180` § The design).
- **The Dock icon**: Finder and Launchpad show `Stev.icns`, but the running app's Dock icon didn't. eframe sets egui's own icon at runtime (`setApplicationIconImage`) unless the window is given an empty one. Fixed on the same `fix/` branch as the scan child.
- **Output that goes nowhere** (for phase 5): every `eprintln!` and every panic message. The `eprintln!` sites are a failed settings save (`core/settings.rs`), a MIDI input or output that fails to connect (`threads/midi_watcher.rs`), a failed MIDI send (`threads/midi_output.rs`), the controller thread's MIDI failures (`threads/controller.rs`), a VST3 bundle that fails to load (`vst3/discovery.rs`), the scan child's own errors (`vst3/child.rs`, already discarded by the parent), a CLAP preset not restored (`clap/engine.rs`), and a plugin that fails to load or isn't installed (`view/display/instrument.rs`, five sites). The last is the one a user meets: a project whose plugin isn't installed opens without a word. There is no panic hook, so a crash leaves nothing behind either.

### 3. The About tab, with Check for updates

A fourth settings tab, `SettingsTab::About`, after Keyboard:

- The name, the version (`CARGO_PKG_VERSION`), the architecture the binary is running as (useful in an Apple Silicon bug report), the licence line, and a link to stevmidi.com and the repo.
- The VST Compatible logo (light or dark variant from the theme's brightness) with the sentence "VST is a registered trademark of Steinberg Media Technologies GmbH." (`080` § Licensing).
- A **Check for updates** button, with the answer beside it: "You're up to date (0.5.0)", "Version 0.6.0 is available" with a link to the release page, or "Couldn't reach GitHub". While the check runs, the button shows "Checking…" and is disabled.
- **The fetch**: a short-lived thread runs `/usr/bin/curl -fsSL --max-time 10 -H "Accept: application/vnd.github+json" https://api.github.com/repos/stevmidi/stev/releases/latest`. It reads `tag_name` and `html_url` with `serde_json` (already a dependency) and sends the result back over a channel. The UI polls the channel each frame while the tab is open, and the result is repainted with `request_repaint`. Never on the audio or render path.
- **`cfg(target_os = "macos")`**: elsewhere the button isn't shown, since no binary ships there.
- **The app menu goes through the tab.** winit's default app menu has an About item that opens AppKit's stock panel (`orderFrontStandardAboutPanel:`), which would disagree with the tab: no VST logo, no update check. `view/appkit.rs` already rewires Quit. These items are rewired or added the same way, from `route_quit_through_window_close` (renamed to suit, e.g. `wire_app_menu`), when the app is created:
  - **About Stev** (the existing item, retargeted) opens Settings on the About tab.
  - **Check for Updates…** (new, under About) opens Settings on the About tab **and starts the check**, as if the button had been pressed. If a check is already running, it just opens the tab.
  - **Settings…** (new, with the ⌘, shortcut, after a separator) opens Settings on the last tab used, as ⌘, does today. A menu item's shortcut is matched before the key reaches egui, so ⌘, now arrives through the menu. Nothing changes for the user, and the keymap's fixed ⌘, row stays, since off macOS (and in a bare `cargo run` without the menu) egui still sees the key.
  - **The route**: each item targets a method added at runtime to winit's delegate class (as `applicationShouldTerminate:` is added in `hold_terminate`). The method stores a request in an `AtomicU8` (none / about / check / settings; as built, a main-thread `Cell`, since the menu and the frame share the main thread) and calls `request_repaint` on a stored `egui::Context`. `Display` takes the request each frame and opens the modal, as the `?` chip does. No lock, no channel on the render path.
  - **The app's name in the items**: winit titles them with the process name, i.e. the executable's file name, so they would read "About stev", "Quit stev". Phase 2 confirms this inside a bundle. If it holds, the bundle's executable is named `Stev` (`CFBundleExecutable`, phase 1's script), rather than retitling items one by one.
- **Tests** (`core/update_check.rs`, pure): the version comparison (strips `v`, numeric `major.minor.patch`, an unparseable tag reads as "couldn't check", never "newer"), and parsing the response JSON, including a missing `tag_name`.
- Docs: `030` (the tab), `010` (the tab's keys, if any, and ⌘, arriving through the menu on macOS), `020` § Views (the tab list), and `060` § Unsaved changes, which points to `appkit.rs` for the menu wiring.

**As built** (2026-10-24), where it differs from or adds to the plan above:

- **The executable is `Stev`** in the bundle (`bundle.sh`, the plist template). The cargo binary stays `stev`.
- **The answer is taken every frame, not only while the tab is open** (`Display::sync_update_check`): a check started from the menu and the tab then closed still lands, and the `try_recv` costs nothing while no check runs. A click, `Enter` or the menu only asks for a check (`UpdateState::Requested`); the frame starts it, since it has the egui context the thread wakes.
- **`Enter` on the About tab checks**, as the button does. The tab has no other keys.
- **One gate for the platform**: `UPDATE_CHECK` (`cfg!(target_os = "macos")`) decides whether the button is drawn, hit and keyed. The check's code and state compile, and are linted, on every platform; nothing else in the tab is `cfg`-gated.
- **Settings… tells its key from a click**: when the event that sent the action is a key press (`NSApplication::currentEvent`), the request is `SettingsKey`, which `Display` adds to egui's raw input as the `⌘,` key itself, pressed and released (`App::raw_input_hook`, beside the file-drag pointer AppKit also keeps from egui), so the chord keeps every behaviour it had (toggling, ignored under a text field or dialog). A click opens Settings on the last tab, or leaves it up.
- **A clicked menu item waits for nothing**: About and Check for Updates… do nothing while a project dialog, a text field or the restore overlay has the keyboard (`keyboard_taken`), rather than queueing.
- **Links open with `open`** (`explorer`, `xdg-open` off macOS): eframe is built without egui-winit's `links` feature, and the opener is a process that exits at once. Each link is a full-width row, as every row in the modal is a click target.
- **The logo** is `docs/images/vst/*.rgba`, 48 and 96 px high, from `scripts/make-vst-logo.sh` (resvg, then python3's standard library to unpack the PNG). The app reads each width back from its file's length.
- **Hand test** (2026-10-24, Intel Mac, the bundle installed from a quarantined zip): the menu reads "About Stev" and "Quit Stev" with Check for Updates… and Settings… under About; About and Check for Updates… open the tab, also with Settings already up; `⌘,` toggles as before and a click on Settings… opens it; a check answers "You're up to date (0.5.0)", and "Couldn't reach GitHub" with Wi-Fi off; the links open in the browser; the logo is sharp; a menu item does nothing under Save As. All as intended. Not tested: the "available" answer and its release link, since no release is newer than 0.5.0.

### 4. Opening projects from Finder

Double-clicking a `.stev` (or dropping one on the Dock icon) opens it. Needs phase 3's request route.

- **The plist**: a `CFBundleDocumentTypes` entry (role Editor, `LSHandlerRank` Owner) for an exported type, plus `UTExportedTypeDeclarations` declaring `com.stevmidi.stev.project` (conforming to `public.data`, extension `stev`, description "Stev Project"). Optionally a document icon, made by the same icon script.
- **Catching the open, including at launch**: macOS delivers the open-documents Apple Event (`kAEOpenDocuments`), which winit doesn't pass on. When the double-click is what launches Stev, the event arrives during launch, before eframe has created the window and before `appkit.rs` wires anything today. So the handler is installed in `main()`, before eframe runs, with `NSAppleEventManager::setEventHandler…` for `kCoreEventClass`/`kAEOpenDocuments` (installing it before launch is how Apple says to receive launch-time opens). Adding `application:openURLs:` to winit's delegate at app creation would miss the launch case.
- **Into the app**: the handler stores the path (the first `.stev` of the event, others ignored) in a main-thread slot and calls `request_repaint` once a `Context` exists. `Display` takes it each frame, and once the app is up, it goes through the same load path as the browser's Enter on a `.stev`, including the unsaved-changes check when the open project is dirty. A path that arrives at launch waits until the first frame and replaces the startup project.
- **What it doesn't do**: no recent-documents list, no opening `.mid` from Finder (import stays a drag or the browser), no second window.
- **Test**: the path-taking and dirty-check routing is pure enough for a test in `view/` (a pending open while dirty asks first, while clean loads). The rest is hand-tested inside the bundle: cold launch by double-click, double-click while running (clean and dirty), drop on the Dock icon, a file whose load fails.
- Docs: `060` (opening from Finder and the dirty check), `000` (the handler installed in `main()`).

### 5. The log file

The shape agreed 2026-09-24 (memory: logging deferred until distribution):

- **The `log` facade**, with `release_max_level_info`, plus a small backend writing to stderr **and** `~/Library/Logs/stev/stev.log` (beside the overrun journal). The backend is `simplelog` or `fern`, whichever passes `cargo deny` with fewer crates. The file is rotated once at launch: the previous run is kept as `stev.1.log`, so a crash report can attach the run that crashed.
- **Migration**: `dprintln!` → `debug!`, `eprintln!` → `error!`/`warn!`, kept `println!`s → `info!` (about 70 `dprintln!` sites, plus the errors). The `dprintln!` ground rule in `AGENTS.md` becomes "`log` macros, never `println!`", and `080` changes to match.
- **Never from the audio thread**: the overrun journal's lock-free ring and drain thread stay the pattern there (`170`).
- **Plugin stdout noise** (plugins that write to fd 1 directly): in a bundle it goes nowhere, which is fine. Redirecting fd 1/2 into the log is not attempted.
- **The About tab gains "Show log in Finder"** (`open -R`), and the bug-report form asks for the file.
- On Linux and Windows, the file goes in the platform's equivalent (`dirs::data_local_dir()/stev/logs`), so CI exercises the same code.

### 6. The release workflow

`.github/workflows/release.yml`, on `push: tags: ['v*']`, one macOS job:

- Checks out the tag, runs `scripts/macos/bundle.sh --version ${tag#v}`, and fails if the tag and `Cargo.toml`'s version disagree.
- Uploads the zip to the tag's release with `gh release upload --clobber`. If no release exists yet, it creates a **draft** with `gh release create --draft`, so the author still writes and publishes the notes (they carry the VST logo and trademark sentence, `080` § Releasing). A SHA-256 of the zip is printed and attached as `….zip.sha256`.
- `permissions: contents: write`, and nothing else. No secrets until level 2.
- Same change: the README's **Status** line and a new **Download** section (the zip, *Open Anyway* with phase 2's exact wording, "Apple Silicon untested"), `080` § Releasing (the workflow attaches the app, so the release is published after it finishes), `000` § CI (the new workflow), and a `CHANGELOG.md` line.
- **First real run**: the next version tag after this merges. Before that, test it on a throwaway tag in a fork, or with `workflow_dispatch` to a draft. Tags on `stevmidi/stev` are never deleted, so no test tags go there.

## What changes in the docs

- `280`: the candidate points here, and loses the items as each phase lands. Level 1 leaves the list when phase 6 merges, and levels 2 and 3 stay.
- `000` § CI: the release workflow. § Build: the bundle script.
- `030`, `020`: the About tab.
- `080`: § Releasing (the app is attached by CI) and § Conventions (logging, phase 5).
- `AGENTS.md`: the logging ground rule (phase 5) and this doc's row in the index.
- README: Status and Download (phase 6).

## Not in scope

- Signing with a Developer ID, notarization, the hardened runtime and its `disable-library-validation` entitlement, a `.dmg` (level 2).
- A Homebrew cask (level 3).
- A self-updater (Sparkle), or an automatic update check.
- **More menus than the app menu** (File, Edit, View, Window). Decided 2026-10-24: the app menu holds About, Check for Updates, Settings, Hide and Quit, and nothing else. A menu item's shortcut is matched before egui sees the key, so menu chords would take keys from the keymap. After a rebind, a menu would run the old command or show the wrong chord (`320`), unless it were rebuilt on every keymap change. The Keyboard tab is the one command reference, and egui handles the clipboard in text fields without an Edit menu. If a File menu is wanted later (e.g. Open and Save for discoverability), it is generated from the keymap table and shows the current chords without registering them as shortcuts, so the keymap still owns every key.
- Linux or Windows binaries (`280` § Linux and Windows support, stage 4).
