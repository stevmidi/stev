//! A minimal native `NSWindow` to host an **embedded** plugin editor, whatever
//! the format.
//!
//! Most plugins (u-he, JUCE-based, ...) only support embedded GUIs, not floating
//! ones, so the host has to provide a parent view. This is the smallest thing
//! that works: a plain titled window whose `contentView` is handed to the
//! plugin (CLAP `gui.set_parent`, VST3 `IPlugView::attached`). Plugin-initiated
//! resizes (e.g. zooming the editor) are forwarded here via `set_content_size`
//! when the plugin asks for one — from the CLAP editor's `pump` after
//! `request_resize`, from inside VST3's `IPlugFrame::resizeView`. Still no
//! delegate / the other direction: the window is not user-resizable (no
//! `Resizable` style mask), so there is nothing to forward back to the plugin.
//!
//! The window floats above the main app window ([`PluginWindow::set_level`],
//! driven from each editor's `pump` through [`editor_level`]) while Stev
//! is the active application, so adjusting the main window doesn't bury the
//! editor; it drops back to a normal level when the user switches to another
//! app, and behind the main window while that shows a dialog the user must
//! answer, so the dialog isn't hidden behind it.
//!
//! The window is destroyed together with the plugin's view when the editor is
//! closed (see [`InstrumentEditor::close`](super::editor::InstrumentEditor::close)), so its
//! on-screen position is not preserved by the window itself: the editor
//! remembers [`PluginWindow::frame_top_left`] before tearing it down and hands
//! it back to [`PluginWindow::new`] on the next open.
//!
//! [`FALLBACK_SIZE`] and [`editor_title`] are the rest of the window policy the
//! formats share.
//!
//! Must be constructed and used on the main thread (`NSWindow` is
//! `MainThreadOnly`); [`PluginWindow::new`] returns `None` off the main thread.

use core::ffi::c_void;
use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSFloatingWindowLevel, NSNormalWindowLevel, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::key_guard;

/// Editor content size for a plugin that does not report one — the same for
/// every format, so an unreporting plugin opens the same size whichever it is.
pub(super) const FALLBACK_SIZE: (f64, f64) = (900.0, 600.0);

/// Editor window title: `"<plugin> — Track N"`, one-based.
pub(super) fn editor_title(plugin_name: &str, track: usize) -> String {
    format!("{plugin_name} — Track {}", track + 1)
}

/// Where an editor window sits against the main window — set each frame by
/// the editor's `pump` ([`editor_level`]), applied by
/// [`PluginWindow::set_level`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EditorLevel {
    /// Above every normal window (`NSFloatingWindowLevel`): shown while the
    /// user works in Stev, so adjusting the main window doesn't bury it.
    Float,
    /// An ordinary window (`NSNormalWindowLevel`): Stev isn't the
    /// frontmost app, so it doesn't hover over unrelated windows — or it
    /// isn't shown.
    Normal,
    /// An ordinary window sent to the back: the main window shows a dialog
    /// the user must answer (the unsaved-changes prompt, Save As), which the
    /// editor would otherwise cover.
    Behind,
}

/// The level for an editor window — `shown` (a hidden CLAP editor never
/// floats), and `yield_to_main` while the main window shows a dialog.
/// Floating needs Stev to be the frontmost (active) application; `false`
/// off the main thread (never expected — the editor pump is main-thread
/// only).
pub(super) fn editor_level(shown: bool, yield_to_main: bool) -> EditorLevel {
    let active =
        MainThreadMarker::new().is_some_and(|mtm| NSApplication::sharedApplication(mtm).isActive());
    if yield_to_main {
        EditorLevel::Behind
    } else if shown && active {
        EditorLevel::Float
    } else {
        EditorLevel::Normal
    }
}

define_class!(
    /// The editor window's delegate, there only to turn the title-bar close
    /// button (and ⌘W) into a request: it records the click and answers "not
    /// yet", and the editor's `pump` tears the plugin's GUI down while the
    /// window is still on screen, then drops it.
    ///
    /// Letting AppKit close the window outright took it off screen at once and
    /// left a slow plugin (Kontakt takes seconds) tearing down behind an
    /// invisible window, with Stev frozen and no sign why. This way the click
    /// behaves like `v`: the window stays up, with the busy cursor, until the
    /// plugin is done.
    // SAFETY: `NSObject` has no subclassing requirements, and this class
    // implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StevPluginWindowCloseInterceptor"]
    #[ivars = Cell<bool>]
    struct CloseInterceptor;

    unsafe impl NSObjectProtocol for CloseInterceptor {}

    unsafe impl NSWindowDelegate for CloseInterceptor {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().set(true);
            // The click arrived in the editor window, which eframe never
            // hears about, so nothing else would run the pump that acts on it.
            key_guard::wake_ui();
            false
        }
    }
);

impl CloseInterceptor {
    /// A fresh delegate with no close requested.
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(false));
        // SAFETY: `init` is `NSObject`'s designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Owns a native window for the plugin's embedded editor view.
pub(super) struct PluginWindow {
    /// The native window the plugin view is parented into.
    window: Retained<NSWindow>,
    /// The window's delegate. `NSWindow` holds its delegate weakly, so this is
    /// what keeps it alive.
    close_interceptor: Retained<CloseInterceptor>,
    /// Last level applied by [`set_level`](Self::set_level), so the per-frame
    /// pump only calls into AppKit when it actually changes.
    level: Cell<EditorLevel>,
}

impl PluginWindow {
    /// Creates a hidden, titled window with a `width × height` content area.
    /// Call [`show`](Self::show) after `gui.set_parent` + `gui.show`.
    ///
    /// `top_left` reopens the window where the user last left it (from
    /// [`frame_top_left`](Self::frame_top_left) before the previous teardown);
    /// `None` — the first open of a plugin — centers it.
    pub(super) fn new(
        title: &str,
        width: f64,
        height: f64,
        top_left: Option<NSPoint>,
    ) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;

        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;

        // SAFETY: standard NSWindow designated initializer, valid args, on the
        // main thread (proven by `mtm`).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };

        window.setTitle(&NSString::from_str(title));
        match top_left {
            Some(origin) => window.setFrameTopLeftPoint(origin),
            None => window.center(),
        }
        // The plugin owns the view lifecycle via `gui.destroy`; keep the Rust
        // `Retained` as the sole owner of the window itself.
        unsafe { window.setReleasedWhenClosed(false) };
        let close_interceptor = CloseInterceptor::new(mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(&*close_interceptor)));

        // Registers this window with the Space/`v` key guard (see
        // `key_guard`) so those keys stay reserved for the app even once the
        // user clicks into this window and it takes OS keyboard focus.
        key_guard::register_window(Retained::as_ptr(&window) as usize);

        Some(Self {
            window,
            close_interceptor,
            level: Cell::new(EditorLevel::Normal),
        })
    }

    /// The window's top-left corner in screen coordinates, stashed by the
    /// editor before a teardown so the next open lands in the same spot.
    /// Top-left rather than the frame's (bottom-left) origin because the plugin
    /// may come back at a different size, and it is the title bar the user
    /// positioned.
    pub(super) fn frame_top_left(&self) -> NSPoint {
        let frame = self.window.frame();
        NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height)
    }

    /// Raw `NSView*` of the window's content view, to parent the plugin's view
    /// into (CLAP `gui.set_parent`, VST3 `IPlugView::attached`). The window keeps
    /// the view alive; the pointer is valid until this `PluginWindow` drops.
    pub(super) fn content_view_ptr(&self) -> Option<*mut c_void> {
        let view = self.window.contentView()?;
        Some(Retained::as_ptr(&view) as *mut c_void)
    }

    /// Resizes the content area to match the plugin's requested size. Called
    /// once right after parenting (the plugin's initial size) and again
    /// whenever the plugin later asks for a resize (e.g. on zoom).
    ///
    /// The window's top-left corner is held fixed across the resize, so a
    /// zooming editor grows downward from where the user put it instead of
    /// walking up the screen — and so restoring a remembered position before
    /// the plugin's post-parent size arrives stays exact.
    pub(super) fn set_content_size(&self, width: f64, height: f64) {
        let top_left = self.frame_top_left();
        self.window.setContentSize(NSSize::new(width, height));
        self.window.setFrameTopLeftPoint(top_left);
    }

    /// Puts the window at `level` (see [`EditorLevel`]). Cheap to call every
    /// frame — only touches AppKit when the level changes. [`Behind`] lowers
    /// the window and sends it to the back too: lowering alone leaves it on
    /// top of the normal-level windows, the main window included (and floating
    /// again brings it back above them).
    ///
    /// [`Behind`]: EditorLevel::Behind
    pub(super) fn set_level(&self, level: EditorLevel) {
        if self.level.replace(level) == level {
            return;
        }
        self.window.setLevel(match level {
            EditorLevel::Float => NSFloatingWindowLevel,
            EditorLevel::Normal | EditorLevel::Behind => NSNormalWindowLevel,
        });
        // `orderBack` would also order in a hidden window, so only a shown one.
        if level == EditorLevel::Behind && self.window.isVisible() {
            self.window.orderBack(None);
        }
    }

    /// Brings the window to the front **without taking keyboard focus** — the
    /// main app window keeps key focus so the `v` toggle keeps working. The user
    /// clicks the plugin window when they want to interact with it.
    pub(super) fn show(&self) {
        self.window.orderFront(None);
    }

    /// Whether the window is currently on screen. The title-bar close button
    /// doesn't take it off — see [`take_closed`](Self::take_closed).
    pub(super) fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    /// Whether the user has closed the editor since the last call, so its GUI
    /// should be torn down: the title-bar close button was clicked — the
    /// window is then still on screen, and the caller tears the plugin's GUI
    /// down before dropping this, which closes it (see [`CloseInterceptor`])
    /// — or the window left the screen by some other route.
    pub(super) fn take_closed(&self) -> bool {
        self.close_interceptor.ivars().replace(false) || !self.is_visible()
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        key_guard::unregister_window(Retained::as_ptr(&self.window) as usize);
        self.window.setDelegate(None);
        self.window.close();
    }
}
