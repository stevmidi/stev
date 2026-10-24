# A Downloadable macOS App

**Status: plan agreed 2026-10-24 on `feature/macos-app-bundle`; nothing built yet.** Level 1 of the `280` candidate *A downloadable macOS app*: an unsigned universal `Stev.app` in a zip, attached to each `v*` release, with the items that come with it (the hand test, the About tab with its update check, opening projects from Finder, a log file). Levels 2 (signed + notarized) and 3 (a Homebrew cask) stay in `280`. Read `280` § A downloadable macOS app first, and `080` § Releasing and § Licensing.

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
  - `Contents/Info.plist`: generated from a template (`assets/macos/Info.plist.in`), with the version from `Cargo.toml`. It sets `CFBundleIdentifier` `com.stevmidi.stev`, `CFBundleName`/`CFBundleDisplayName` `Stev`, `CFBundleExecutable` `stev`, `CFBundleIconFile`, `NSHighResolutionCapable`, `LSMinimumSystemVersion`, `NSHumanReadableCopyright`, `LSApplicationCategoryType` `public.app-category.music`.
  - `Contents/MacOS/stev`: the universal binary.
  - `Contents/Resources/`: `Stev.icns`, `LICENSE-MIT`, `LICENSE-APACHE`, `THIRD-PARTY-NOTICES.md`, the Plex `OFL.txt` (the font is compiled in, so its licence travels with the binary), and the VST logo's `LICENSE.txt`.
- **An ad-hoc signature over the whole bundle**: `codesign --force --sign - Stev.app`. This step is required, not cosmetic. Apple Silicon refuses to run an unsigned arm64 binary, and `lipo` plus the bundle edits can invalidate the linker's own signature. It doesn't make Gatekeeper trust the app (that is level 2), it just lets the app run at all.
- **Zipped with `ditto -c -k --keepParent`**, not `zip`, which loses the bundle's extended attributes and symlinks.
- `scripts/macos/make-icon.sh` builds `Stev.icns` from `assets/macos/icon.svg` with `rsvg-convert` and `iconutil`. It runs by hand when the icon changes, and its output is committed.
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
  - **The route**: each item targets a method added at runtime to winit's delegate class (as `applicationShouldTerminate:` is added in `hold_terminate`). The method stores a request in an `AtomicU8` (none / about / check / settings) and calls `request_repaint` on a stored `egui::Context`. `Display` takes the request each frame and opens the modal, as the `?` chip does. No lock, no channel on the render path.
  - **The app's name in the items**: winit titles them with the process name, i.e. the executable's file name, so they would read "About stev", "Quit stev". Phase 2 confirms this inside a bundle. If it holds, the bundle's executable is named `Stev` (`CFBundleExecutable`, phase 1's script), rather than retitling items one by one.
- **Tests** (`core/update_check.rs`, pure): the version comparison (strips `v`, numeric `major.minor.patch`, an unparseable tag reads as "couldn't check", never "newer"), and parsing the response JSON, including a missing `tag_name`.
- Docs: `030` (the tab), `010` (the tab's keys, if any, and ⌘, arriving through the menu on macOS), `020` § Views (the tab list), and `060` § Unsaved changes, which points to `appkit.rs` for the menu wiring.

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
