//! macOS AppKit glue for the main window: its native content view
//! ([`content_view`], also what the file-drag pointer reads), the app menu
//! ([`wire_app_menu`]), and every way of quitting (⌘Q, Quit from the Dock,
//! logging out) made to close the main window instead of terminating the
//! app, so quitting goes through the unsaved-changes check.
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
//! The app menu's About item would open AppKit's stock About panel, which
//! knows nothing of the About tab; it is pointed at the tab instead, and
//! Check for Updates… and Settings… are added under it. Each targets a method
//! added to the delegate's class the same way, which stores an
//! [`AppMenuRequest`] for `Display` to take on the next frame and wakes it.
//! See `330-macos-app.md` § The About tab.
//!
//! Also [`MainGlContext`]: eframe's OpenGL context, kept so it can be made
//! current again after plugin code has made its own current.

use std::cell::RefCell;
use std::ffi::CStr;
use std::sync::atomic::{AtomicU8, Ordering};

use eframe::CreationContext;
use egui::Context;
use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, sel};
// Deprecated by Apple in favour of Metal, but it is what eframe's glow
// renderer draws with.
#[allow(deprecated)]
use objc2_app_kit::NSOpenGLContext;
use objc2_app_kit::{
    NSApplication, NSApplicationTerminateReply, NSEventType, NSMenu, NSMenuItem, NSView, NSWindow,
};
use objc2_foundation::NSString;
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
    /// (main thread) egui's context, woken when a menu item stores a request.
    static REPAINT_CTX: RefCell<Option<Context>> = RefCell::default();
}

/// What an app-menu item asked for, taken by `Display` each frame
/// ([`take_app_menu_request`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMenuRequest {
    /// About Stev: Settings on the About tab.
    About = 1,
    /// Check for Updates…: the About tab, and a check started.
    CheckForUpdates,
    /// Settings… clicked: Settings on the tab last used.
    Settings,
    /// Settings…'s `⌘,`, which the menu matches before egui sees the key:
    /// handed on as the key itself.
    SettingsKey,
}

impl AppMenuRequest {
    /// Every request, in discriminant order from 1.
    const ALL: [AppMenuRequest; 4] = [
        AppMenuRequest::About,
        AppMenuRequest::CheckForUpdates,
        AppMenuRequest::Settings,
        AppMenuRequest::SettingsKey,
    ];
}

/// The last request a menu item stored, `0` for none. A later one replaces
/// it: the frame between them acts on the latest.
static APP_MENU_REQUEST: AtomicU8 = AtomicU8::new(0);

/// The request an app-menu item stored since the last call, if any.
pub(crate) fn take_app_menu_request() -> Option<AppMenuRequest> {
    let stored = APP_MENU_REQUEST.swap(0, Ordering::Relaxed);
    AppMenuRequest::ALL
        .into_iter()
        .find(|&request| request as u8 == stored)
}

/// Stores `request` for the next frame and wakes it.
fn send_app_menu_request(request: AppMenuRequest) {
    APP_MENU_REQUEST.store(request as u8, Ordering::Relaxed);
    REPAINT_CTX.with(|ctx| {
        if let Some(ctx) = &*ctx.borrow() {
            ctx.request_repaint();
        }
    });
}

/// Wires the app menu: routes every quit through the new window's
/// `performClose:` — the menu's Quit item (the one sending `terminate:`) is
/// pointed at it, and the app delegate gets an `applicationShouldTerminate:`
/// that cancels a `terminate:` from elsewhere (the Dock, a logout) and closes
/// the window instead — and points About at the About tab, adding Check for
/// Updates… and Settings… under it. Call once, from eframe's app-creation
/// closure: winit has built its menu and set its delegate by then. Skips any
/// part that is missing.
pub(crate) fn wire_app_menu(cc: &CreationContext<'_>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(window) = content_view(cc).and_then(|view| view.window()) else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    MAIN_WINDOW.with(|main| *main.borrow_mut() = Weak::from_retained(&window));
    REPAINT_CTX.with(|ctx| *ctx.borrow_mut() = Some(cc.egui_ctx.clone()));
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
    if let Some(app_menu) = items.firstObject().and_then(|item| item.submenu()) {
        add_about_items(&app, &app_menu, mtm);
    }
}

/// Points the app menu's About item at the About tab, and adds Check for
/// Updates… under it, then a separator and Settings… (`⌘,`). Each item
/// targets the app delegate, whose class gains the method it sends
/// ([`add_delegate_method`]). Nothing changes if there is no About item.
fn add_about_items(app: &NSApplication, app_menu: &NSMenu, mtm: MainThreadMarker) {
    let Some(delegate) = app.delegate() else {
        return;
    };
    let delegate_object: &AnyObject = delegate.as_ref();
    let Some(about) = app_menu
        .itemArray()
        .iter()
        .find(|item| item.action() == Some(sel!(orderFrontStandardAboutPanel:)))
    else {
        return;
    };
    // SAFETY: the delegate's class, which each method is added to before an
    // item can send it.
    let class = unsafe { object_getClass(delegate_object) };
    add_delegate_method(class, sel!(stevShowAbout:), show_about);
    add_delegate_method(class, sel!(stevCheckForUpdates:), check_for_updates);
    add_delegate_method(class, sel!(stevShowSettings:), show_settings);
    let new_item = |title: &str, action: Sel, key: &str| {
        // SAFETY: a fresh item with the action the delegate now answers; the
        // target is held weakly, and winit's event loop keeps the delegate
        // for as long as the app runs.
        unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            );
            item.setTarget(Some(delegate_object));
            item
        }
    };
    // SAFETY: as for the new items.
    unsafe {
        about.setTarget(Some(delegate_object));
        about.setAction(Some(sel!(stevShowAbout:)));
    }
    let at = app_menu.indexOfItem(&about);
    app_menu.insertItem_atIndex(
        &new_item("Check for Updates…", sel!(stevCheckForUpdates:), ""),
        at + 1,
    );
    app_menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), at + 2);
    // ⌘ is a key equivalent's modifier unless another is set.
    app_menu.insertItem_atIndex(&new_item("Settings…", sel!(stevShowSettings:), ","), at + 3);
}

/// An action method: `void (id self, SEL _cmd, id sender)`.
type ActionImp = extern "C-unwind" fn(&AnyObject, Sel, Option<&AnyObject>);

/// Adds `imp` to `class` as the action method `sel`.
fn add_delegate_method(class: *const AnyClass, sel: Sel, imp: ActionImp) {
    /// `void (id self, SEL _cmd, id sender)`.
    const TYPES: &CStr = c"v@:@";
    // SAFETY: `imp` has the C signature `TYPES` describes, the one a menu
    // item sends its action with; erasing it to `Imp` is how the runtime
    // stores every method. `class_addMethod` adds nothing if the class
    // already has the selector.
    unsafe {
        let imp = std::mem::transmute::<ActionImp, Imp>(imp);
        class_addMethod(class.cast_mut(), sel, imp, TYPES.as_ptr());
    }
}

/// About Stev's action ([`add_about_items`]).
extern "C-unwind" fn show_about(_this: &AnyObject, _cmd: Sel, _sender: Option<&AnyObject>) {
    send_app_menu_request(AppMenuRequest::About);
}

/// Check for Updates…'s action ([`add_about_items`]).
extern "C-unwind" fn check_for_updates(_this: &AnyObject, _cmd: Sel, _sender: Option<&AnyObject>) {
    send_app_menu_request(AppMenuRequest::CheckForUpdates);
}

/// Settings…'s action ([`add_about_items`]): its `⌘,` when the event that
/// sent it is a key press, a click otherwise.
extern "C-unwind" fn show_settings(_this: &AnyObject, _cmd: Sel, _sender: Option<&AnyObject>) {
    let from_key = MainThreadMarker::new()
        .and_then(|mtm| NSApplication::sharedApplication(mtm).currentEvent())
        .is_some_and(|event| event.r#type() == NSEventType::KeyDown);
    send_app_menu_request(if from_key {
        AppMenuRequest::SettingsKey
    } else {
        AppMenuRequest::Settings
    });
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
