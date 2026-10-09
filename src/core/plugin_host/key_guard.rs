//! App-wide reservation of four bare keys — `Space`, `v`, `.`, `0` — for
//! transport control and editor visibility, even while a hosted plugin's
//! **embedded** editor window has macOS keyboard focus.
//!
//! Once the user clicks into an embedded plugin editor, AppKit gives that
//! window key focus and the eframe window stops receiving keyboard events
//! entirely — `egui`'s own input polling (`InputPoller`) has no way to see
//! these keys at that point, since winit never delivers them to a window
//! that isn't key. The only fix is intercepting the keystroke *before*
//! AppKit routes it to whichever window is currently key: a local
//! [`NSEvent`] monitor observes every key-down in this app and can swallow
//! the ones it wants, regardless of which of our windows they were headed
//! for.
//!
//! This only intercepts while the focused window is one of our own tracked
//! plugin editor windows ([`register_window`] / [`unregister_window`],
//! called by [`super::window::PluginWindow`]'s constructor/`Drop`) —
//! never the main app window, so normal in-focus behavior (the existing
//! egui-based bindings) is completely untouched. It only ever intercepts the
//! bare key (no Shift/Control/Option/Command) — narrower than the in-focus
//! `v` binding, which also accepts Shift/Option/Control — so it never eats a
//! modified chord a plugin's own UI might use. `NumericPad` and `CapsLock`
//! are deliberately *not* treated as blocking modifiers — see the comment at
//! the check itself for why (numpad `0`/`.` is the whole point of this
//! feature; Caps Lock being toggled on would otherwise defeat it entirely).
//!
//! **Tradeoff, unavoidable without introspecting the plugin's own focused
//! control (which this host has no access to):** while an editor window is
//! focused, none of these four can be typed as a literal character into
//! anything inside the plugin's own UI (e.g. a preset-name field) — every
//! other key still reaches the plugin normally.
//!
//! **Only covers embedded editors.** A floating CLAP GUI (rare — see
//! `130-plugin-host.md`; most plugins don't support it) owns its own native
//! window, invisible to this registry, so this fix does not apply to it.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::{Arc, OnceLock};

use block2::RcBlock;
use crossbeam_channel::Sender;
use egui::Key;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

use crate::core::input_event::{InputEvent, KeyModifiers};

thread_local! {
    /// Raw `NSWindow*` pointers (as `usize`) of every currently-open embedded
    /// plugin editor window. Always reflects exactly the windows the app
    /// currently owns — main-thread-only, like everything else here, so a
    /// plain `RefCell` (no `Send`/`Sync` needed) is enough.
    static EDITOR_WINDOWS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// Set when the monitor intercepts a bare `v` while an editor window is
    /// focused; drained once per frame by `Display` via
    /// [`take_toggle_editor_pending`].
    static TOGGLE_EDITOR_PENDING: Cell<bool> = const { Cell::new(false) };
    /// Where an intercepted Space is forwarded — the same channel the normal
    /// egui-driven binding already sends through. `None` until
    /// [`install_key_guard`] runs.
    static INPUT_EVENT_TX: RefCell<Option<Sender<InputEvent>>> = const { RefCell::new(None) };
    /// Wakes the eframe render loop after we act on an intercepted key — see
    /// the note in [`handle_key_event`] on why this is required, not optional.
    static REPAINT_CTX: RefCell<Option<Arc<OnceLock<egui::Context>>>> =
        const { RefCell::new(None) };
    /// Keeps the block and the monitor token alive for the app's lifetime —
    /// dropping either would tear the monitor down.
    static MONITOR: RefCell<Option<MonitorHandle>> = const { RefCell::new(None) };
}

/// Keeps the local-event-monitor token and its block alive; dropping either
/// tears the monitor down.
struct MonitorHandle {
    /// The `addLocalMonitorForEventsMatchingMask:` token.
    _token: Retained<AnyObject>,
    /// The monitor's handler block.
    _block: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>,
}

/// Registers `window` (an `NSWindow*` as `usize`) as one of our own embedded
/// plugin editor windows.
pub(super) fn register_window(window: usize) {
    EDITOR_WINDOWS.with(|w| w.borrow_mut().push(window));
}

/// Reverses [`register_window`].
pub(super) fn unregister_window(window: usize) {
    EDITOR_WINDOWS.with(|w| w.borrow_mut().retain(|&p| p != window));
}

/// Whether `window` is one of our registered embedded editor windows.
fn is_editor_window(window: usize) -> bool {
    EDITOR_WINDOWS.with(|w| w.borrow().contains(&window))
}

/// Forwards a bare key press into the same channel — and so the same
/// dispatch (`EventHandlers::handle_input_event`) — a normal in-focus
/// keypress already goes through.
fn send_key_pressed(key: Key) {
    INPUT_EVENT_TX.with(|tx| {
        if let Some(tx) = tx.borrow().as_ref() {
            tx.send(InputEvent::KeyPressed {
                key,
                modifiers: KeyModifiers::default(),
            })
            .ok();
        }
    });
}

/// Drains the "toggle the selected track's editor" flag set by the key
/// monitor. Call once per frame; the caller still owns the guards the
/// in-focus `v` binding gets for free (the arranger has the keyboard, the
/// settings modal is closed), since this module has no access to `Display`'s
/// state.
pub(crate) fn take_toggle_editor_pending() -> bool {
    TOGGLE_EDITOR_PENDING.with(|f| f.replace(false))
}

/// Installs the app-wide key guard. Call once, from the `eframe::run_native`
/// creation closure (after AppKit/`NSApplication` is up) — never before.
pub(crate) fn install_key_guard(
    input_event_tx: Sender<InputEvent>,
    repaint_ctx: Arc<OnceLock<egui::Context>>,
) {
    INPUT_EVENT_TX.with(|tx| *tx.borrow_mut() = Some(input_event_tx));
    REPAINT_CTX.with(|ctx| *ctx.borrow_mut() = Some(repaint_ctx));

    let block = RcBlock::new(handle_key_event);

    // SAFETY: the block's return is always either the untouched event
    // pointer or null, as `addLocalMonitorForEventsMatchingMask:handler:`
    // requires.
    let token = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };

    match token {
        Some(token) => MONITOR.with(|m| {
            *m.borrow_mut() = Some(MonitorHandle {
                _token: token,
                _block: block,
            })
        }),
        None => dprintln!("plugin host: failed to install the key guard monitor"),
    }
}

/// The monitor's handler: returns the event unchanged to let it through
/// normally, or `null` to swallow it.
fn handle_key_event(event: NonNull<NSEvent>) -> *mut NSEvent {
    let raw = event.as_ptr();
    // SAFETY: AppKit guarantees a valid event for the duration of this call.
    let event = unsafe { event.as_ref() };

    if event.isARepeat() {
        return raw;
    }
    // Only the modifiers that actually change what a bare keypress means —
    // deliberately *not* `DeviceIndependentFlagsMask` (Apple's usual
    // "any meaningful modifier" pattern), which also bundles in `NumericPad`
    // and `CapsLock`. `NumericPad` matters here specifically: the classic DAW
    // transport pairing this guards (numpad `0` = play, numpad `.` = stop)
    // always carries that flag, and `CapsLock` reflects the toggle's current
    // state on *every* keypress, not just while physically held — either one
    // in the reject set would have silently defeated the guard for anyone
    // using the numpad or with Caps Lock on.
    const RELEVANT_MODIFIERS: NSEventModifierFlags = NSEventModifierFlags::Shift
        .union(NSEventModifierFlags::Control)
        .union(NSEventModifierFlags::Option)
        .union(NSEventModifierFlags::Command);
    if !event
        .modifierFlags()
        .intersection(RELEVANT_MODIFIERS)
        .is_empty()
    {
        return raw;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return raw;
    };
    let Some(window) = event.window(mtm) else {
        return raw;
    };
    if !is_editor_window(Retained::as_ptr(&window) as usize) {
        return raw;
    }
    // `charactersIgnoringModifiers` (not `keyCode`, which is a US-layout
    // virtual key code) so this respects the user's actual keyboard layout.
    let Some(chars) = event.charactersIgnoringModifiers() else {
        return raw;
    };

    match chars.to_string().as_str() {
        " " => send_key_pressed(Key::Space),
        // Stop — the classic DAW numpad transport shortcut pair with `0`
        // below (both always carry `NumericPad` on a real numpad, already
        // excluded above).
        "." => send_key_pressed(Key::Period),
        // Play from cursor.
        "0" => send_key_pressed(Key::Num0),
        "v" => TOGGLE_EDITOR_PENDING.with(|f| f.set(true)),
        _ => return raw,
    }

    // Pushing onto `input_event_tx` / setting the pending flag only queues
    // the work — it's drained on `Display`'s next `update()` call, which
    // eframe only runs in response to an actual window/repaint event. Unlike
    // a normal keypress (which arrives *as* such an event), ours came from
    // outside eframe's own input pipeline entirely, so nothing wakes it: if
    // the app happens to be idle right now (nothing animating, unfocused),
    // the queued action sits untouched until something else — a stray mouse
    // move over the main window, say — happens to trigger a frame. Request
    // one explicitly instead of leaving that to chance; this is exactly what
    // `repaint_ctx` exists for (see `EventHandlers::request_repaint`).
    REPAINT_CTX.with(|ctx| {
        if let Some(ctx) = ctx.borrow().as_ref().and_then(|c| c.get()) {
            ctx.request_repaint();
        }
    });

    std::ptr::null_mut()
}
