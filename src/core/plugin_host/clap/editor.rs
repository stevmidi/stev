//! The UI-thread driver for one hosted plugin's editor window.
//!
//! [`ClapEditor`] owns the `!Send` [`PluginInstance`] for a track's instrument
//! (it must stay on the eframe main thread) and is pumped once per frame from
//! `Display::update`.
//!
//! It prefers a **floating** window (plugin-owned), but most plugins only
//! support **embedded** GUIs, so it falls back to parenting the plugin into a
//! plain native [`PluginWindow`]. The GUI is created lazily on first show
//! (`v`) and **destroyed again on close**, the way mainstream hosts treat a
//! closed plugin window: a merely hidden editor keeps the plugin's own repaint
//! timers running and costs idle CPU for a UI nobody is looking at. Only the
//! view is destroyed — parameters, preset and the audio processor are untouched
//! — and [`ClapEditor::window_top_left`] carries the window's position across
//! the cycle so reopening lands where the user left it.

use std::ffi::CString;
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, PluginGui, Window as ClapWindow};
use clack_host::prelude::*;
use objc2_foundation::NSPoint;

use super::host::StevClapHost;
use crate::core::plugin_host::editor::InstrumentEditor;
use crate::core::plugin_host::window::{FALLBACK_SIZE, PluginWindow, editor_level, editor_title};

/// GUI config for a plugin-owned floating window.
const COCOA_FLOATING: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::COCOA,
    is_floating: true,
};
/// GUI config for a view embedded in our own window.
const COCOA_EMBEDDED: GuiConfiguration<'static> = GuiConfiguration {
    api_type: GuiApiType::COCOA,
    is_floating: false,
};

/// Owns one hosted plugin instance and drives its editor window.
pub(crate) struct ClapEditor {
    /// The hosted plugin instance.
    instance: PluginInstance<StevClapHost>,
    /// Editor window title (`"<plugin> — Track N"`).
    title: String,
    /// Whether `gui.create` succeeded and `gui.destroy` is owed.
    created: bool,
    /// Whether the editor window is currently shown.
    visible: bool,
    /// `Some` only for an embedded editor — the native window the plugin's view
    /// is parented into. Must outlive `gui.destroy`.
    window: Option<PluginWindow>,
    /// Screen position of the embedded editor window at its last teardown, so
    /// the next open reopens it in place instead of re-centering. `None` until
    /// the window has been opened and closed once.
    window_top_left: Option<NSPoint>,
}

impl ClapEditor {
    /// Wraps an instance; the editor window is not created until
    /// [`show`](Self::show).
    pub(crate) fn new(
        instance: PluginInstance<StevClapHost>,
        plugin_name: &str,
        track: usize,
    ) -> Self {
        Self {
            instance,
            title: editor_title(plugin_name, track),
            created: false,
            visible: false,
            window: None,
            window_top_left: None,
        }
    }
}

impl InstrumentEditor for ClapEditor {
    /// Shows the editor, creating it on first call. No-op on failure — the
    /// instrument still plays, just with no UI.
    fn show(&mut self) {
        if !self.created {
            self.create();
        }
        if !self.created {
            return;
        }
        if let Some(gui) = self.instance.access_shared_handler(|s| s.plugin_gui()) {
            let plugin = self.instance.plugin_handle();
            gui.show(&plugin).ok();
        }
        if let Some(window) = &self.window {
            window.show();
        }
        self.visible = true;
    }

    fn is_open(&self) -> bool {
        self.visible
    }

    /// Destroys the plugin GUI and closes its window, remembering where the
    /// window sat. Does **not** touch the audio processor — for a track whose
    /// plugin is being replaced or cleared the caller must have removed the
    /// voice from the mixer first; for an ordinary [`close`](Self::close) the
    /// instrument keeps playing, just without a UI.
    fn teardown_gui(&mut self) {
        if let Some(window) = &self.window {
            self.window_top_left = Some(window.frame_top_left());
        }
        if self.created
            && let Some(gui) = self.instance.access_shared_handler(|s| s.plugin_gui())
        {
            let plugin = self.instance.plugin_handle();
            gui.destroy(&plugin);
        }
        self.created = false;
        self.visible = false;
        self.window = None;
    }

    /// Captures the plugin's current CLAP `state` blob (active preset / knob
    /// positions) for persistence. `None` if the plugin has no `state`
    /// extension or the save call fails — the track is then persisted without a
    /// preset and reloads at the plugin's default.
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let state_ext = self.instance.access_shared_handler(|s| s.plugin_state())?;
        let mut blob = Vec::new();
        let plugin = self.instance.plugin_handle();
        match state_ext.save(&plugin, &mut blob) {
            Ok(()) if !blob.is_empty() => Some(blob),
            Ok(()) => None,
            Err(_e) => {
                dprintln!("clap host: plugin state save failed: {_e}");
                None
            }
        }
    }

    /// Finalises teardown after the mixer has acked that this track's voice was
    /// dropped: deactivates the now-processorless instance so its `Drop` is a
    /// clean destroy rather than a leak.
    fn deactivate(&mut self) {
        // The `StoppedPluginAudioProcessor` handle has been dropped on the audio
        // thread; a few retries cover the tiny window before that is visible here.
        for _ in 0..100 {
            if self.instance.try_deactivate().is_ok() {
                return;
            }
            std::thread::yield_now();
        }
    }

    /// Per-frame pump: run any requested main-thread callback, tick the plugin's
    /// registered timers while visible, and react to the window being closed.
    ///
    /// Returns the interval the UI should schedule its next repaint at to keep
    /// servicing this editor, or `None` if it needs no periodic servicing.
    fn pump(&mut self, yield_to_main: bool) -> Option<Duration> {
        if !self.created {
            return None;
        }

        if self
            .instance
            .access_shared_handler(|s| s.take_callback_request())
        {
            self.instance.call_on_main_thread_callback();
        }

        // Embedded GUI only — a floating GUI owns its own OS window and
        // resizes it directly, so there's no `self.window` to update here.
        if let Some((width, height)) = self
            .instance
            .access_shared_handler(|s| s.take_resize_request())
            && let Some(window) = &self.window
        {
            window.set_content_size(f64::from(width), f64::from(height));
        }

        // The plugin closed its own (floating) window. Whether or not it also
        // destroyed the view we owe it a `destroy` call, and we rebuild from
        // scratch on the next `show`.
        if self
            .instance
            .access_shared_handler(|s| s.take_gui_closed())
            .is_some()
        {
            self.teardown_gui();
            return None;
        }

        // Embedded case: closing our `NSWindow` with its title-bar button never
        // reaches the plugin, so reconcile our `visible` flag with the window.
        // The window stays up while the plugin tears down and goes with it —
        // as with `v`.
        if self.visible
            && self
                .window
                .as_ref()
                .is_some_and(|w| w.take_close_request() || !w.is_visible())
        {
            self.teardown_gui();
            return None;
        }

        // Keep an embedded editor window above the main app window while it is
        // shown and Stev is the active app, so interacting with the main
        // window (track volume, transport, ...) doesn't send the editor behind
        // it — unless the main window shows a dialog. Idempotent — only calls
        // into AppKit on a change.
        if let Some(window) = &self.window {
            window.set_level(editor_level(self.visible, yield_to_main));
        }

        if !self.visible {
            return None;
        }

        let due = self
            .instance
            .access_handler(|mt| mt.due_timers(Instant::now()));
        if !due.is_empty()
            && let Some(timer) = self.instance.access_shared_handler(|s| s.plugin_timer())
        {
            let plugin = self.instance.plugin_handle();
            for id in due {
                timer.on_timer(&plugin, id);
            }
        }

        self.instance.access_handler(|mt| mt.min_timer_period())
    }
}

impl ClapEditor {
    /// Creates the plugin's editor (floating if supported, else embedded in a
    /// native window), once. Leaves `created` false on any failure.
    fn create(&mut self) {
        let Some(gui) = self.instance.access_shared_handler(|s| s.plugin_gui()) else {
            dprintln!("clap host: plugin exposes no gui extension");
            return;
        };

        let floating_supported = {
            let plugin = self.instance.plugin_handle();
            gui.is_api_supported(&plugin, COCOA_FLOATING)
        };

        if floating_supported {
            self.create_floating(gui);
        } else {
            self.create_embedded(gui);
        }
    }

    /// Creates a plugin-owned floating editor window.
    fn create_floating(&mut self, gui: PluginGui) {
        let plugin = self.instance.plugin_handle();
        if let Err(_e) = gui.create(&plugin, COCOA_FLOATING) {
            dprintln!("clap host: gui.create (floating) failed: {_e}");
            return;
        }
        gui.suggest_title(&plugin, &to_cstring(&self.title));
        self.created = true;
        dprintln!("clap host: plugin editor created (floating)");
    }

    /// Creates the plugin view embedded in one of our own native windows (for
    /// plugins that don't support a floating GUI).
    fn create_embedded(&mut self, gui: PluginGui) {
        {
            let plugin = self.instance.plugin_handle();
            if !gui.is_api_supported(&plugin, COCOA_EMBEDDED) {
                dprintln!(
                    "clap host: plugin supports neither a floating nor an embedded cocoa gui"
                );
                return;
            }
            if let Err(_e) = gui.create(&plugin, COCOA_EMBEDDED) {
                dprintln!("clap host: gui.create (embedded) failed: {_e}");
                return;
            }
        }

        let (w, h) = {
            let plugin = self.instance.plugin_handle();
            gui.get_size(&plugin)
                .map(|s| (f64::from(s.width), f64::from(s.height)))
                .unwrap_or(FALLBACK_SIZE)
        };

        let Some(window) = PluginWindow::new(&self.title, w, h, self.window_top_left) else {
            dprintln!("clap host: could not create plugin window");
            let plugin = self.instance.plugin_handle();
            gui.destroy(&plugin);
            return;
        };
        let Some(view_ptr) = window.content_view_ptr() else {
            dprintln!("clap host: plugin window has no content view");
            let plugin = self.instance.plugin_handle();
            gui.destroy(&plugin);
            return;
        };

        {
            let plugin = self.instance.plugin_handle();
            // SAFETY: `view_ptr` is the content `NSView` of `window`, which is
            // moved into `self.window` below and kept alive until `gui.destroy`.
            let result = unsafe {
                let parent = ClapWindow::from_cocoa_nsview(view_ptr);
                gui.set_parent(&plugin, parent)
            };
            if let Err(_e) = result {
                dprintln!("clap host: gui.set_parent failed: {_e}");
                gui.destroy(&plugin);
                return;
            }
        }

        // Some plugins only report a meaningful size once parented.
        if let Some(size) = {
            let plugin = self.instance.plugin_handle();
            gui.get_size(&plugin)
        } {
            window.set_content_size(f64::from(size.width), f64::from(size.height));
        }

        self.window = Some(window);
        self.created = true;
        dprintln!("clap host: plugin editor created (embedded, {w:.0}x{h:.0})");
    }
}

impl Drop for ClapEditor {
    fn drop(&mut self) {
        // Best-effort GUI teardown for exit / project-switch paths. Audio-thread
        // coordination is the caller's job (global `HostShutdown` on app exit,
        // the voice-dropped ack on a per-track remove).
        if self.created
            && let Some(gui) = self.instance.access_shared_handler(|s| s.plugin_gui())
        {
            let plugin = self.instance.plugin_handle();
            gui.destroy(&plugin);
        }
    }
}

/// `&str` → `CString`, falling back to a placeholder if `s` contains a NUL.
fn to_cstring(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::new("Stev plugin").unwrap())
}
