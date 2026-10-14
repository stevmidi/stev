//! macOS AppKit glue for the main window: its native content view
//! ([`content_view`], also what the file-drag pointer reads), and every way
//! of quitting (⌘Q, Quit from the Dock, logging out) made to close the main
//! window instead of terminating the app, so quitting goes through the
//! unsaved-changes check.
//!
//! winit's default app menu wires Quit to `-[NSApplication terminate:]`,
//! which exits without the window ever seeing a close request — eframe gets
//! no chance to hold it. Pointed at the main window's `performClose:`
//! instead, ⌘Q arrives as the same close request the window's close button
//! sends, which `Display::sync_window_close` holds for the check and lets
//! through once approved; closing the main window quits. Quitting from the
//! Dock, or by logging out, calls `terminate:` itself, which asks the app
//! delegate's `applicationShouldTerminate:` first. winit's delegate has none,
//! so one is added to its class at runtime: it cancels the terminate and
//! closes the main window the same way. See `060-persistence.md` § Unsaved
//! changes.
//!
//! Also [`MainGlContext`]: eframe's OpenGL context, kept so it can be made
//! current again after plugin code has made its own current.

use std::cell::RefCell;
use std::ffi::CStr;

use eframe::CreationContext;
use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
// Deprecated by Apple in favour of Metal, but it is what eframe's glow
// renderer draws with.
#[allow(deprecated)]
use objc2_app_kit::NSOpenGLContext;
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply, NSView, NSWindow};
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

thread_local! {
    /// (main thread) The main window, which a cancelled `terminate:` closes
    /// instead ([`should_terminate`]).
    static MAIN_WINDOW: RefCell<Weak<NSWindow>> = RefCell::default();
}

/// Routes every quit through the new window's `performClose:`: points the
/// app menu's Quit item (the one sending `terminate:`) at it, and gives the
/// app delegate an `applicationShouldTerminate:` that cancels a `terminate:`
/// from elsewhere (the Dock, a logout) and closes the window instead. Call
/// once, from eframe's app-creation closure: winit has built its menu and
/// set its delegate by then. Skips any part that is missing.
pub(crate) fn route_quit_through_window_close(cc: &CreationContext<'_>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(window) = content_view(cc).and_then(|view| view.window()) else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    MAIN_WINDOW.with(|main| *main.borrow_mut() = Weak::from_retained(&window));
    hold_terminate(&app);
    let Some(menu) = app.mainMenu() else {
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

/// Adds [`should_terminate`] to the app delegate's class (winit's, which has
/// no `applicationShouldTerminate:` of its own), then sets the delegate again
/// so AppKit sees the new method even if it cached what the delegate answers.
fn hold_terminate(app: &NSApplication) {
    let Some(delegate) = app.delegate() else {
        return;
    };
    let delegate_object: &AnyObject = delegate.as_ref();
    /// `NSApplicationTerminateReply (id self, SEL _cmd, NSApplication *sender)`.
    const TYPES: &CStr = c"Q@:@";
    // SAFETY: `should_terminate` has the C signature `TYPES` describes, the
    // one AppKit calls `applicationShouldTerminate:` with; erasing it to `Imp`
    // is how the runtime stores every method. `class_addMethod` adds nothing
    // if the class already has the selector.
    let added = unsafe {
        let class = object_getClass(delegate_object).cast_mut();
        let imp = std::mem::transmute::<
            extern "C-unwind" fn(
                &AnyObject,
                Sel,
                Option<&AnyObject>,
            ) -> NSApplicationTerminateReply,
            Imp,
        >(should_terminate);
        class_addMethod(
            class,
            sel!(applicationShouldTerminate:),
            imp,
            TYPES.as_ptr(),
        )
    };
    if added.as_bool() {
        // The delegate is strongly held by winit's event loop (AppKit's
        // reference is weak) and by `delegate` here, so it survives the swap.
        app.setDelegate(None);
        app.setDelegate(Some(&delegate));
    }
}

/// `-applicationShouldTerminate:` for the app delegate ([`hold_terminate`]):
/// a `terminate:` from the Dock or a logout is cancelled and becomes a close
/// of the main window, which the unsaved-changes check holds, so the app ends
/// the way ⌘Q ends it. Lets the terminate through if the window is gone.
extern "C-unwind" fn should_terminate(
    _this: &AnyObject,
    _cmd: Sel,
    _sender: Option<&AnyObject>,
) -> NSApplicationTerminateReply {
    match MAIN_WINDOW.with(|main| main.borrow().load()) {
        Some(window) => {
            window.performClose(None);
            NSApplicationTerminateReply::TerminateCancel
        }
        None => NSApplicationTerminateReply::TerminateNow,
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
