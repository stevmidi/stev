//! macOS AppKit glue for the main window: its native content view
//! ([`content_view`], also what the file-drag pointer reads), and the app
//! menu's Quit (⌘Q) made to close the main window instead of terminating the
//! app, so quitting goes through the unsaved-changes check.
//!
//! winit's default app menu wires Quit to `-[NSApplication terminate:]`,
//! which exits without the window ever seeing a close request — eframe gets
//! no chance to hold it. Pointed at the main window's `performClose:`
//! instead, ⌘Q arrives as the same close request the window's close button
//! sends, which `Display::sync_window_close` holds for the check and lets
//! through once approved; closing the main window quits. Quitting from the
//! Dock, or by logging out, still calls `terminate:` and skips the check —
//! winit's app delegate implements no `applicationShouldTerminate:` to
//! intercept it. See `060-persistence.md` § Unsaved changes.
//!
//! Also [`MainGlContext`]: eframe's OpenGL context, kept so it can be made
//! current again after plugin code has made its own current.

use eframe::CreationContext;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, sel};
// Deprecated by Apple in favour of Metal, but it is what eframe's glow
// renderer draws with.
#[allow(deprecated)]
use objc2_app_kit::NSOpenGLContext;
use objc2_app_kit::{NSApplication, NSView};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// eframe's content view of the new window, retained, or `None` without an
/// AppKit one.
pub(crate) fn content_view(cc: &CreationContext<'_>) -> Option<Retained<NSView>> {
    let RawWindowHandle::AppKit(handle) = cc.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: a live AppKit view; retaining it keeps it valid for as long as
    // the caller holds it.
    unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }
}

/// Points the app menu's Quit item (the one sending `terminate:`) at the new
/// window's `performClose:`. Call once, from eframe's app-creation closure:
/// winit has built its menu by then. Does nothing if any part is missing.
pub(crate) fn route_quit_through_window_close(cc: &CreationContext<'_>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let (Some(window), Some(menu)) = (
        content_view(cc).and_then(|view| view.window()),
        NSApplication::sharedApplication(mtm).mainMenu(),
    ) else {
        return;
    };
    let items = menu.itemArray();
    let quit_items = items
        .iter()
        .filter_map(|item| item.submenu())
        .flat_map(|submenu| submenu.itemArray().to_vec())
        .filter(|item| item.action() == Some(sel!(terminate:)));
    for item in quit_items {
        // SAFETY: `performClose:` is an `NSWindow` action taking the sender,
        // as a menu item sends it. The target is held weakly; the main window
        // lives as long as the app.
        unsafe {
            item.setTarget(Some(&window));
            item.setAction(Some(sel!(performClose:)));
        }
    }
}

/// eframe's own `NSOpenGLContext`, made current again at the end of every
/// frame: plugin code (Qt-based editors such as Kontakt's) leaves its own
/// context current, and eframe never notices. See `130-plugin-host.md`
/// § Main-thread GL context.
pub(crate) struct MainGlContext {
    /// The context, retained.
    #[allow(deprecated)]
    context: Retained<NSOpenGLContext>,
}

#[allow(deprecated)]
impl MainGlContext {
    /// The context current right now, which inside eframe's app-creation
    /// closure is eframe's own. `None` if none is current.
    pub(crate) fn capture_current() -> Option<Self> {
        NSOpenGLContext::currentContext().map(|context| Self { context })
    }

    /// Makes the context current again if something else is. One query when
    /// nothing changed.
    pub(crate) fn restore(&self) {
        let current = NSOpenGLContext::currentContext();
        if current.is_none_or(|c| Retained::as_ptr(&c) != Retained::as_ptr(&self.context)) {
            self.context.makeCurrentContext();
        }
    }
}
