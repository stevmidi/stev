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
//! [`AppMenuRequest`] for `Display` to take on the next frame and wakes it —
//! both on the main thread, so a thread-local cell carries it.
//! See `330-macos-app.md` § The About tab.
//!
//! A `.stev` double-clicked in Finder, or dropped on the Dock icon, arrives
//! as the open-documents Apple Event, which winit doesn't pass on.
//! [`install_open_documents_handler`] catches it, at launch too, and keeps
//! the path for `Display` ([`take_open_document`]). See `330-macos-app.md`
//! § Opening projects from Finder.
//!
//! Also [`MainGlContext`]: eframe's OpenGL context, kept so it can be made
//! current again after plugin code has made its own current.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::path::PathBuf;

use eframe::CreationContext;
use egui::Context;
use objc2::ffi::{class_addMethod, object_getClass};
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Imp, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
// Deprecated by Apple in favour of Metal, but it is what eframe's glow
// renderer draws with.
#[allow(deprecated)]
use objc2_app_kit::NSOpenGLContext;
use objc2_app_kit::{
    NSApplication, NSApplicationTerminateReply, NSApplicationWillFinishLaunchingNotification,
    NSEventType, NSMenu, NSMenuItem, NSView, NSWindow,
};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::core::project::is_project_file;

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
    /// (main thread) The last request a menu item stored. A later one
    /// replaces it: the frame between them acts on the latest.
    static APP_MENU_REQUEST: Cell<Option<AppMenuRequest>> = const { Cell::new(None) };
    /// (main thread) The `.stev` Finder last asked to open, until `Display`
    /// takes it. A later one replaces it.
    static OPEN_DOCUMENT: RefCell<Option<PathBuf>> = RefCell::default();
}

/// What an app-menu item asked for, taken by `Display` each frame
/// ([`take_app_menu_request`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMenuRequest {
    /// About Stev: Settings on the About tab.
    About,
    /// Check for Updates…: the About tab, and a check started.
    CheckForUpdates,
    /// Settings… clicked: Settings on the tab last used.
    Settings,
    /// Settings…'s `⌘,`, which the menu matches before egui sees the key:
    /// handed on as the key itself.
    SettingsKey,
}

/// The request an app-menu item stored since the last call, if any.
pub(crate) fn take_app_menu_request() -> Option<AppMenuRequest> {
    APP_MENU_REQUEST.take()
}

/// Stores `request` for the next frame and wakes it.
fn send_app_menu_request(request: AppMenuRequest) {
    APP_MENU_REQUEST.set(Some(request));
    wake_frame();
}

/// Wakes the next frame, once [`wire_app_menu`] has stored egui's context.
/// Before that, the first frame is still to come.
fn wake_frame() {
    REPAINT_CTX.with(|ctx| {
        if let Some(ctx) = &*ctx.borrow() {
            ctx.request_repaint();
        }
    });
}

/// The `.stev` Finder asked to open since the last call, if any. `Display`
/// calls it only when it can act on it, so a path that arrives while a
/// dialog is up waits here.
pub(crate) fn take_open_document() -> Option<PathBuf> {
    OPEN_DOCUMENT.take()
}

/// `kCoreEventClass`.
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
/// `kAEOpenDocuments`.
const OPEN_DOCUMENTS_EVENT: u32 = u32::from_be_bytes(*b"odoc");
/// `keyDirectObject`: the open-documents event's list of files.
const DIRECT_OBJECT_KEYWORD: u32 = u32::from_be_bytes(*b"----");

define_class!(
    /// Receives the open-documents Apple Event. Installed as its handler
    /// once AppKit is about to finish launching
    /// ([`install_open_documents_handler`]).
    // SAFETY: `NSObject` has no subclassing requirements, and this class
    // implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StevOpenDocuments"]
    struct OpenDocuments;

    impl OpenDocuments {
        /// `NSApplicationWillFinishLaunchingNotification`: makes this the
        /// open-documents handler. AppKit installs its own while it
        /// launches, which would replace one installed earlier, and
        /// dispatches a launch-time open after this notification.
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            // SAFETY: `self` answers the selector with an Apple Event
            // handler's signature (the event, then the reply) and lives for
            // the rest of the run (never released). The event
            // class and ID are `FourCharCode`s, `u32`s.
            unsafe {
                let _: () = msg_send![
                    &manager,
                    setEventHandler: self,
                    andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                    forEventClass: CORE_EVENT_CLASS,
                    andEventID: OPEN_DOCUMENTS_EVENT
                ];
            }
        }

        /// The open-documents event: keeps its first `.stev` for `Display`
        /// and wakes it. Others in the same event are ignored.
        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            if let Some(path) = first_project_file(event) {
                OPEN_DOCUMENT.set(Some(path));
                wake_frame();
            }
        }
    }
);

/// The first `.stev` among the open-documents event's files: its direct
/// object, a list of file references (counted from 1), or a lone one.
fn first_project_file(event: &NSAppleEventDescriptor) -> Option<PathBuf> {
    // SAFETY: `paramDescriptorForKeyword:` takes an `AEKeyword` (a `u32`)
    // and returns a descriptor or nil.
    let files: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT_KEYWORD] };
    let files = files?;
    let count = files.numberOfItems();
    let items: Vec<_> = if count == 0 {
        vec![files]
    } else {
        (1..=count)
            .filter_map(|i| files.descriptorAtIndex(i))
            .collect()
    };
    items
        .iter()
        .filter_map(|item| item.fileURLValue()?.to_file_path())
        .find(|path| is_project_file(path))
}

/// Has Finder's opens reach the app ([`OpenDocuments`]): observes AppKit's
/// will-finish-launching notification, which then installs the handler.
/// Call from `main()`, before eframe runs: a double-click that launches Stev
/// delivers its event during the launch, before the window or the app menu
/// exist.
pub(crate) fn install_open_documents_handler() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let this = OpenDocuments::alloc(mtm).set_ivars(());
    // SAFETY: `init` is `NSObject`'s designated initializer.
    let handler: Retained<OpenDocuments> = unsafe { msg_send![super(this), init] };
    let observer: &AnyObject = handler.as_ref();
    // SAFETY: `handler` answers the selector, taking the notification. The
    // centre holds it unretained, and it is never released (below).
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            observer,
            sel!(applicationWillFinishLaunching:),
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
        );
    }
    // Kept for the whole run: neither the notification centre nor the Apple
    // Event manager retains it.
    std::mem::forget(handler);
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
/// ([`add_action_method`]). Nothing changes if there is no About item.
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
    add_action_method(delegate_object, sel!(stevShowAbout:), show_about);
    add_action_method(
        delegate_object,
        sel!(stevCheckForUpdates:),
        check_for_updates,
    );
    add_action_method(delegate_object, sel!(stevShowSettings:), show_settings);
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

/// Adds `imp` to `delegate`'s class (winit's) as the method `sel`, with the
/// Objective-C type encoding `types`; returns whether it was added
/// (`class_addMethod` adds nothing if the class already has the selector).
///
/// # Safety
///
/// `imp` must be a function erased to `Imp` (how the runtime stores every
/// method) whose C signature is the one `types` describes.
unsafe fn add_delegate_method(delegate: &AnyObject, sel: Sel, imp: Imp, types: &CStr) -> bool {
    // SAFETY: the class of a live object; the caller vouches for `imp`.
    unsafe {
        let class = object_getClass(delegate).cast_mut();
        class_addMethod(class, sel, imp, types.as_ptr()).as_bool()
    }
}

/// Adds the menu action `imp` to `delegate`'s class as `sel`
/// ([`add_delegate_method`]).
fn add_action_method(delegate: &AnyObject, sel: Sel, imp: ActionImp) {
    // SAFETY: an `ActionImp` has the C signature `v@:@` describes, the one a
    // menu item sends its action with.
    unsafe {
        add_delegate_method(
            delegate,
            sel,
            std::mem::transmute::<ActionImp, Imp>(imp),
            c"v@:@",
        );
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
    /// `NSApplicationTerminateReply (id self, SEL _cmd, NSApplication *sender)`.
    type TerminateImp =
        extern "C-unwind" fn(&AnyObject, Sel, Option<&AnyObject>) -> NSApplicationTerminateReply;
    // SAFETY: `should_terminate` has the C signature `Q@:@` describes, the
    // one AppKit calls `applicationShouldTerminate:` with.
    let added = unsafe {
        add_delegate_method(
            delegate.as_ref(),
            sel!(applicationShouldTerminate:),
            std::mem::transmute::<TerminateImp, Imp>(should_terminate),
            c"Q@:@",
        )
    };
    if added {
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
