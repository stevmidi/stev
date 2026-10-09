//! The main-thread half of a hosted VST3 plugin: its `!Send` COM objects, and
//! its editor window.
//!
//! A VST3 editor is an `IPlugView` the edit controller creates on demand. The
//! host gives it a native parent view to live in, tells it about an
//! [`IPlugFrame`](HostPlugFrame) so it can ask to be resized, and takes it away
//! again on close. The window itself is the same
//! [`PluginWindow`](crate::core::plugin_host::window) the CLAP host embeds
//! into, so the two formats share the window plumbing, the remembered position
//! and the app-wide key guard.
//!
//! The shapes differ where the APIs do:
//!
//! - **There is no floating option.** CLAP plugins may own their own OS window;
//!   a VST3 view is always parented into one of ours, which makes this the
//!   simpler of the two paths.
//! - **Resize requests are answered on the spot.** CLAP's `request_resize` can
//!   come from anywhere and is stashed in atomics for the next frame; VST3's
//!   `IPlugFrame::resizeView` is a UI-thread call the plugin expects to have
//!   been carried out — window resized, `onSize` delivered — by the time it
//!   returns.
//!
//! See `docs/180-vst3-host.md`.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use objc2_foundation::NSPoint;
use vst3::Steinberg::Vst::ViewType::kEditor;
use vst3::Steinberg::Vst::{
    IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait,
};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, IPluginBaseTrait, ViewRect,
    kInvalidArgument, kPlatformTypeNSView, kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use crate::core::plugin_host::editor::InstrumentEditor;
use crate::core::plugin_host::window::{FALLBACK_SIZE, PluginWindow, editor_level, editor_title};

use super::component::MainThreadHalf;
use super::state;

/// The `IPlugFrame` a plugin view is given, so it can ask its host to resize
/// the window around it (an editor zoom control, a Kontakt instrument with a
/// wider panel, ...).
///
/// The request is carried out **before `resizeView` returns**: the window is
/// resized, then the view is told what it got with `onSize` — the order the
/// spec gives, and what Steinberg's own editor host does. Plugins rely on it.
/// Deferring both to the next frame left Kontakt drawing at its old width in
/// the newly widened window until a mouse move made it lay out again.
///
/// The frame only holds a [`Weak`] handle to the window: the editor owns it,
/// and the window is shared rather than borrowed so the plugin calling in from
/// inside its own code never aliases the editor.
struct HostPlugFrame {
    /// The window the view is parented into, set just before `attached`.
    /// Dead whenever the editor is closed.
    window: RefCell<Weak<PluginWindow>>,
    /// Set while a resize is being applied, so a plugin that asks again from
    /// inside its own `onSize` is refused rather than recursing.
    resizing: Cell<bool>,
}

impl Class for HostPlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for HostPlugFrame {
    unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        if self.resizing.get() {
            return kResultFalse;
        }
        let Some(window) = self.window.borrow().upgrade() else {
            return kResultFalse;
        };
        // SAFETY: the plugin passes a `ViewRect` it owns for the duration of
        // this call; it is only read here, and the values are copied out.
        let (width, height) = rect_size(&unsafe { *new_size });
        dprintln!("vst3: plugin asked to resize to {width:.0}x{height:.0}");

        self.resizing.set(true);
        window.set_content_size(width, height);
        // SAFETY: `view` is the plugin's own view, live for the duration of
        // the call it is making; `rect` is a local of the matching type.
        if let Some(view) = unsafe { ComRef::from_raw(view) } {
            let mut rect = size_rect(width, height);
            unsafe { view.onSize(&mut rect) };
        }
        self.resizing.set(false);
        kResultOk
    }
}

/// One loaded VST3 plugin's main-thread state. `!Send` by construction — see
/// [`MainThreadHalf`].
pub(super) struct Vst3Editor {
    /// The component, its controller and the host objects they point at.
    plugin: MainThreadHalf,
    /// Window title — the plugin name plus its track number.
    title: String,
    /// The plugin's view, once created. `None` whenever the editor is closed:
    /// closing **destroys** the view rather than hiding it, because a
    /// hidden-but-alive editor keeps its animation timers running and
    /// measurably raises idle CPU (learned the hard way with CLAP).
    view: Option<ComPtr<IPlugView>>,
    /// The native window the view is parented into, alive exactly as long as
    /// [`view`](Self::view). Shared only with [`frame`](Self::frame), weakly,
    /// so dropping it here still closes the window.
    window: Option<Rc<PluginWindow>>,
    /// The `IPlugFrame` handed to the view. Held for the view's lifetime
    /// because the plugin keeps a raw pointer to it.
    frame: ComWrapper<HostPlugFrame>,
    /// Where the window's title bar was when it was last closed, so the next
    /// open lands in the same place instead of re-centering.
    window_top_left: Option<NSPoint>,
    /// Set once the plugin has been terminated, so `deactivate` and `Drop`
    /// don't do it twice.
    finished: bool,
}

impl Vst3Editor {
    /// Wraps the main-thread half of a freshly loaded plugin. The editor itself
    /// is not created until the first [`show`](InstrumentEditor::show).
    pub(super) fn new(plugin: MainThreadHalf, plugin_name: &str, track: usize) -> Self {
        Self {
            plugin,
            title: editor_title(plugin_name, track),
            view: None,
            window: None,
            frame: ComWrapper::new(HostPlugFrame {
                window: RefCell::new(Weak::new()),
                resizing: Cell::new(false),
            }),
            window_top_left: None,
            finished: false,
        }
    }

    /// Creates the plugin's view and parents it into a fresh window. Leaves
    /// both `None` on any failure, so the instrument still plays without a UI.
    fn create(&mut self) {
        let Some(controller) = &self.plugin.controller else {
            dprintln!("vst3: plugin has no edit controller, so no editor");
            return;
        };

        // SAFETY: `controller` is live and on its own (main) thread. Each call
        // below is made in the order `IPlugView` requires — platform check,
        // size query, `setFrame`, `attached` — and every out-parameter is a
        // local of the matching type whose `tresult` is checked before use.
        unsafe {
            let Some(view) = ComPtr::from_raw(controller.createView(kEditor)) else {
                dprintln!("vst3: plugin exposes no editor view");
                return;
            };
            if view.isPlatformTypeSupported(kPlatformTypeNSView) != kResultOk {
                dprintln!("vst3: plugin's editor does not support NSView");
                return;
            }

            let (w, h) = view_size(&view).unwrap_or(FALLBACK_SIZE);
            let Some(window) = PluginWindow::new(&self.title, w, h, self.window_top_left) else {
                dprintln!("vst3: could not create plugin window");
                return;
            };
            let Some(view_ptr) = window.content_view_ptr() else {
                dprintln!("vst3: plugin window has no content view");
                return;
            };
            let window = Rc::new(window);
            *self.frame.window.borrow_mut() = Rc::downgrade(&window);

            // `setFrame` before `attached`, so a view that wants to resize
            // itself during attach has somewhere to say so.
            if let Some(frame) = self.frame.to_com_ptr::<IPlugFrame>() {
                view.setFrame(frame.as_ptr());
            }

            // SAFETY: `view_ptr` is `window`'s content `NSView`, and `window`
            // is moved into `self.window` below and kept alive until
            // `view.removed()` runs in `teardown_gui`.
            if view.attached(view_ptr, kPlatformTypeNSView) != kResultOk {
                dprintln!("vst3: the plugin's editor refused to attach");
                view.setFrame(std::ptr::null_mut());
                return;
            }

            // Some plugins only report a meaningful size once attached. The
            // view is told what it got, just as after a `resizeView`.
            let attached_size = view_size(&view);
            if let Some((aw, ah)) = attached_size
                && (aw, ah) != (w, h)
            {
                window.set_content_size(aw, ah);
                view.onSize(&mut size_rect(aw, ah));
            }

            self.window = Some(window);
            self.view = Some(view);
            dprintln!(
                "vst3: plugin editor created ({w:.0}x{h:.0}, {attached_size:?} once attached)"
            );
        }
    }

    /// Deactivates and terminates the plugin.
    ///
    /// **The order is fixed by the spec and getting it wrong crashes plugins.**
    /// It runs strictly backwards from `load`: stop processing, deactivate,
    /// *disconnect the two halves from each other*, then terminate the
    /// controller, then the component. Terminating a controller while it is
    /// still connected to a live, active component segfaults u-he's plugins
    /// (Repro-1 and Repro-5, reproducibly) — they are entitled to assume the
    /// host tears down in the reverse of the order it built up.
    ///
    /// Idempotent, so `deactivate` and `Drop` can both call it.
    fn terminate(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.teardown_gui();
        // SAFETY: this is the main thread, which owns both objects; both are
        // live (nothing else terminates them) and this runs at most once. The
        // matching voice has already been dropped by the reclaim thread, so no
        // `process` call can be in flight against the same plugin.
        unsafe {
            // The component and the audio processor are the same object seen
            // through two interfaces, so this reaches the processor even though
            // the voice that used to hold it is long gone.
            if let Some(processor) = self.plugin.component.cast::<IAudioProcessor>() {
                processor.setProcessing(0);
            }
            self.plugin.component.setActive(0);

            if let Some(controller) = &self.plugin.controller {
                disconnect(&self.plugin.component, controller);
                controller.terminate();
            }
            self.plugin.component.terminate();
        }
    }
}

impl InstrumentEditor for Vst3Editor {
    fn show(&mut self) {
        if self.view.is_none() {
            self.create();
        }
        if let Some(window) = &self.window {
            window.show();
        }
    }

    fn is_open(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.is_visible())
    }

    fn teardown_gui(&mut self) {
        // Remember where the user put the window before it goes away.
        if let Some(window) = &self.window {
            self.window_top_left = Some(window.frame_top_left());
        }
        if let Some(view) = self.view.take() {
            // SAFETY: `view` is live and attached to `self.window`, which is
            // still alive at this point — `removed` must happen before the
            // parent view goes away. Clearing the frame afterwards drops the
            // plugin's pointer to our `IPlugFrame`.
            unsafe {
                view.removed();
                view.setFrame(std::ptr::null_mut());
            }
        }
        // Dropping the window closes it; it is never merely hidden.
        self.window = None;
    }

    fn save_state(&mut self) -> Option<Vec<u8>> {
        // SAFETY: this is the main thread, which owns both objects, and both
        // are live — `save_state` is only reached while the track still holds
        // this editor, so `terminate` cannot have run.
        unsafe {
            state::capture(
                &self.plugin.component,
                self.plugin.controller.as_ref(),
                self.plugin.controller_is_component,
            )
        }
    }

    fn deactivate(&mut self) {
        self.terminate();
    }

    fn pump(&mut self, yield_to_main: bool) -> Option<Duration> {
        let Some(window) = &self.window else {
            return None;
        };

        // Closing our window with its title-bar button never reaches the
        // plugin, so reconcile: a close request (or a window that is somehow
        // no longer visible) means the editor was closed, and closing
        // destroys. The window is still up while the plugin tears down, and
        // goes with it — as with `v`.
        if window.take_closed() {
            self.teardown_gui();
            return None;
        }

        // Keep the editor above the main window while Stev is active, so
        // clicking the arranger doesn't send the plugin behind it — unless the
        // main window shows a dialog. Idempotent — only calls into AppKit on a
        // change.
        window.set_level(editor_level(true, yield_to_main));

        // Nothing here needs a timed wake-up: VST3 has no host-driven timer
        // extension to service, unlike CLAP. The view drives its own animation
        // off the AppKit run loop.
        None
    }
}

impl Drop for Vst3Editor {
    fn drop(&mut self) {
        // Best-effort teardown for paths that drop without deactivating (a
        // project switch, app exit). Audio-thread coordination is the caller's
        // job — the global `HostShutdown` on exit, the voice-dropped ack on a
        // per-track remove — exactly as for CLAP.
        self.terminate();
    }
}

/// Unwires the component and controller from each other, the exact inverse of
/// the `connect` pair `load` made. Both directions, and before either half is
/// terminated.
///
/// # Safety
///
/// Both must still be live, initialised objects.
unsafe fn disconnect(component: &ComPtr<IComponent>, controller: &ComPtr<IEditController>) {
    let (Some(from), Some(to)) = (
        component.cast::<IConnectionPoint>(),
        controller.cast::<IConnectionPoint>(),
    ) else {
        return;
    };
    // SAFETY: the caller guarantees both are live; each is handed the other's
    // pointer, which is valid because both are still held here.
    unsafe {
        from.disconnect(to.as_ptr());
        to.disconnect(from.as_ptr());
    }
}

/// The plugin view's current size, or `None` if it declines to report one.
///
/// # Safety
///
/// `view` must be a live plugin view.
unsafe fn view_size(view: &ComPtr<IPlugView>) -> Option<(f64, f64)> {
    let mut rect = ViewRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the caller guarantees a live view; `rect` is a local of the
    // matching type for the out-parameter.
    if unsafe { view.getSize(&mut rect) } != kResultOk {
        return None;
    }
    let (w, h) = rect_size(&rect);
    // A plugin that reports a degenerate size gets the fallback instead of a
    // zero-sized window the user cannot see or close.
    (w >= 1.0 && h >= 1.0).then_some((w, h))
}

/// Width and height of a `ViewRect`, in window points. Clamped at zero: the
/// fields are independent `int32`s and nothing stops a plugin reporting a
/// reversed rect.
fn rect_size(rect: &ViewRect) -> (f64, f64) {
    (
        f64::from((rect.right - rect.left).max(0)),
        f64::from((rect.bottom - rect.top).max(0)),
    )
}

/// A `ViewRect` of the given size, anchored at the origin — what `onSize`
/// wants back after the host has resized the window.
fn size_rect(width: f64, height: f64) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ViewRect {
        ViewRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn rect_size_is_the_span_not_the_corners() {
        assert_eq!(rect_size(&rect(0, 0, 800, 600)), (800.0, 600.0));
        // A rect that does not start at the origin still reports its span.
        assert_eq!(rect_size(&rect(10, 20, 810, 620)), (800.0, 600.0));
    }

    #[test]
    fn a_reversed_rect_clamps_to_zero_rather_than_going_negative() {
        // Nothing stops a plugin reporting this, and a negative window size is
        // not something AppKit should ever be handed.
        assert_eq!(rect_size(&rect(800, 600, 0, 0)), (0.0, 0.0));
    }

    #[test]
    fn size_rect_round_trips_through_rect_size() {
        let r = size_rect(1280.0, 720.0);
        assert_eq!(rect_size(&r), (1280.0, 720.0));
        assert_eq!((r.left, r.top), (0, 0));
    }

    fn frame() -> HostPlugFrame {
        HostPlugFrame {
            window: RefCell::new(Weak::new()),
            resizing: Cell::new(false),
        }
    }

    #[test]
    fn a_resize_request_without_a_window_is_refused() {
        // The editor is closed (or not yet open): there is nothing to resize,
        // and saying so beats pretending the view got its size.
        let mut r = rect(0, 0, 640, 480);
        // SAFETY: `r` is a valid local `ViewRect`, as the plugin would pass.
        let result = unsafe { frame().resizeView(std::ptr::null_mut(), &mut r) };
        assert_eq!(result, kResultFalse);
    }

    #[test]
    fn a_resize_request_from_inside_on_size_is_refused() {
        let frame = frame();
        frame.resizing.set(true);
        let mut r = rect(0, 0, 640, 480);
        // SAFETY: `r` is a valid local `ViewRect`.
        let result = unsafe { frame.resizeView(std::ptr::null_mut(), &mut r) };
        assert_eq!(result, kResultFalse);
        // The guard belongs to the outer resize, which clears it.
        assert!(frame.resizing.get());
    }

    #[test]
    fn a_null_resize_request_is_rejected_not_dereferenced() {
        // SAFETY: passing null is exactly the case under test.
        let result = unsafe { frame().resizeView(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(result, kInvalidArgument);
    }
}
