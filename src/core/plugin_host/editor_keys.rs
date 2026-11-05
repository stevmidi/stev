//! How keys get from a hosted plugin's **embedded** editor window to Stev.
//!
//! Once the user clicks into an embedded plugin editor, AppKit gives that
//! window key focus and the eframe window stops receiving keyboard events
//! entirely, so `egui`'s input polling (`InputPoller`) can't see them. Two
//! routes bring them back, the same split a DAW has:
//!
//! - **The plugin goes first for typed keys.** AppKit hands a key to the
//!   plugin's view, and what the plugin doesn't use passes up the responder
//!   chain to the editor window, whose `keyDown:` lands in
//!   [`forward_unused_key`] (`window::EditorWindow`). A plugin's text field
//!   keeps what is typed into it and the host never sees it, so the host needs
//!   no list of keys and no knowledge of the plugin's focused control. Checked
//!   by hand in Serum 2, Spire, Omnisphere, FM8 and Kontakt 8 (`130-plugin-host.md`).
//! - **The project chords don't wait for the plugin.** `⌘S`, `⇧⌘S` and `⌘N`
//!   are Stev's whatever has focus, as a DAW's menu shortcuts are. Stev has no
//!   File menu, so a local [`NSEvent`] monitor takes them before AppKit routes
//!   them ([`handle_key_event`]), and only while the key window is an
//!   [`EditorWindow`] — never the main window, which reads them through egui
//!   as usual.
//!
//! Either way the key is queued here as a raw `KeyPressed` with its real
//! modifiers, and `Display::handle_input_events` adds [`take_editor_keys`] to
//! the frame's events, so a key from an editor goes through the same chain as
//! one typed in the main window: dialogs and text fields first, then the
//! project chords, the overlays and the keymap.
//!
//! **Only covers embedded editors.** A floating CLAP GUI (rare — see
//! `130-plugin-host.md`; most plugins don't support it) owns its own native
//! window, not an [`EditorWindow`], so neither route reaches it.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::{Arc, OnceLock};

use block2::RcBlock;
use egui::Key;
use log::warn;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{ClassType, MainThreadMarker};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

use crate::core::input_event::{InputEvent, KeyModifiers};
use crate::core::keymap::{Chord, PROJECT_CHORDS};

use super::window::EditorWindow;

thread_local! {
    /// Key presses from editor windows, waiting for [`take_editor_keys`].
    static EDITOR_KEYS: RefCell<Vec<InputEvent>> = const { RefCell::new(Vec::new()) };
    /// Wakes the eframe render loop after a key is queued — see the note in
    /// [`queue_key`] on why this is required, not optional.
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

/// Queues a key press for the next frame's input events.
fn queue_key(key: Key, modifiers: KeyModifiers) {
    EDITOR_KEYS.with(|keys| {
        keys.borrow_mut()
            .push(InputEvent::KeyPressed { key, modifiers });
    });
    // Queueing only stores the key: it's read on `Display`'s next frame, which
    // eframe only runs in response to a window or repaint event. Unlike a
    // normal keypress (which arrives *as* such an event), ours came from
    // outside eframe's own input pipeline, so nothing wakes it: with the app
    // idle, the key would sit untouched until something else, a stray mouse
    // move over the main window, say, happened to trigger a frame.
    wake_ui();
}

/// The key presses queued since the last call, for the frame's input events.
/// Main thread only, like the queue.
pub(crate) fn take_editor_keys() -> Vec<InputEvent> {
    EDITOR_KEYS.with(|keys| std::mem::take(&mut *keys.borrow_mut()))
}

/// The key a keyboard's named key is, by `NSEvent` key code: the ones whose
/// characters are control codes or private-use function-key values, which
/// [`Key::from_name`] can't read.
fn named_key(key_code: u16) -> Option<Key> {
    Some(match key_code {
        36 | 76 => Key::Enter,
        48 => Key::Tab,
        51 => Key::Backspace,
        53 => Key::Escape,
        96 => Key::F5,
        97 => Key::F6,
        98 => Key::F7,
        99 => Key::F3,
        100 => Key::F8,
        101 => Key::F9,
        103 => Key::F11,
        109 => Key::F10,
        111 => Key::F12,
        115 => Key::Home,
        116 => Key::PageUp,
        117 => Key::Delete,
        118 => Key::F4,
        119 => Key::End,
        120 => Key::F2,
        121 => Key::PageDown,
        122 => Key::F1,
        123 => Key::ArrowLeft,
        124 => Key::ArrowRight,
        125 => Key::ArrowDown,
        126 => Key::ArrowUp,
        _ => return None,
    })
}

/// The key a press is: a named key by its code, else what was typed, else the
/// key under it. What was typed comes first because `⌥` makes the character
/// on some layouts (`[` is `⌥8` on Nordic ones, where the character ignoring
/// modifiers is `8`); it says nothing for `⌥S` on a US layout (`ß`) or with
/// Control held (a control code), and the character ignoring modifiers does.
fn key_of_press(key_code: u16, typed: Option<&str>, unmodified: Option<&str>) -> Option<Key> {
    named_key(key_code)
        .or_else(|| typed.and_then(Key::from_name))
        .or_else(|| unmodified.and_then(Key::from_name))
}

/// The key and modifiers of a key press. Only the four modifiers that change
/// a key's meaning: `NumericPad` and `CapsLock` are flags too, but numpad `0`
/// is `0`, and Caps Lock is set on every press while it's on.
fn press_of(event: &NSEvent) -> Option<(Key, KeyModifiers)> {
    let typed = event.characters().map(|chars| chars.to_string());
    let unmodified = event
        .charactersIgnoringModifiers()
        .map(|chars| chars.to_string());
    let key = key_of_press(event.keyCode(), typed.as_deref(), unmodified.as_deref())?;
    let flags = event.modifierFlags();
    let modifiers = KeyModifiers {
        shift: flags.contains(NSEventModifierFlags::Shift),
        command: flags.contains(NSEventModifierFlags::Command),
        alt: flags.contains(NSEventModifierFlags::Option),
        ctrl: flags.contains(NSEventModifierFlags::Control),
    };
    Some((key, modifiers))
}

/// Takes a key the plugin passed up the responder chain, from the editor
/// window's `keyDown:`. Returns `false` for a key Stev can't name, to go on
/// to `NSWindow`. An OS repeat is taken and dropped: nothing in an editor
/// window auto-repeats, as the poller's arrows do from the main window.
pub(super) fn forward_unused_key(event: &NSEvent) -> bool {
    let Some((key, modifiers)) = press_of(event) else {
        return false;
    };
    if !event.isARepeat() {
        queue_key(key, modifiers);
    }
    true
}

/// Installs the app-wide project-chord monitor. Call once, from the `eframe::run_native`
/// creation closure (after AppKit/`NSApplication` is up) — never before.
pub(crate) fn install_editor_keys(repaint_ctx: Arc<OnceLock<egui::Context>>) {
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
        None => warn!("plugin host: failed to install the project-chord monitor"),
    }
}

/// The key and modifiers of `event` if it's a project chord headed for an
/// editor window.
fn project_chord_in_editor(event: &NSEvent) -> Option<(Key, KeyModifiers)> {
    if event.isARepeat() {
        return None;
    }
    let window = event.window(MainThreadMarker::new()?)?;
    if !window.isKindOfClass(EditorWindow::class()) {
        return None;
    }
    let (key, modifiers) = press_of(event)?;
    PROJECT_CHORDS
        .contains(&Chord::from_press(key, modifiers))
        .then_some((key, modifiers))
}

/// The monitor's handler: swallows a project chord headed for an editor
/// window by returning `null`, and returns anything else unchanged.
fn handle_key_event(event: NonNull<NSEvent>) -> *mut NSEvent {
    let raw = event.as_ptr();
    // SAFETY: AppKit guarantees a valid event for the duration of this call.
    match project_chord_in_editor(unsafe { event.as_ref() }) {
        Some((key, modifiers)) => {
            queue_key(key, modifiers);
            std::ptr::null_mut()
        }
        None => raw,
    }
}

/// Requests a frame from the eframe render loop, for work queued from outside
/// its own input pipeline — an intercepted key here, a close-button click on
/// an editor window (`window::CloseInterceptor`). A no-op until
/// [`install_editor_keys`] runs.
pub(super) fn wake_ui() {
    REPAINT_CTX.with(|ctx| {
        if let Some(ctx) = ctx.borrow().as_ref().and_then(|c| c.get()) {
            ctx.request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_key_is_read_by_its_code() {
        // Arrow keys type private-use characters, Enter a control code.
        assert_eq!(
            key_of_press(123, Some("\u{f702}"), Some("\u{f702}")),
            Some(Key::ArrowLeft)
        );
        assert_eq!(key_of_press(36, Some("\r"), Some("\r")), Some(Key::Enter));
        assert_eq!(key_of_press(76, Some("\u{3}"), None), Some(Key::Enter));
    }

    /// Numpad `0` has a key code of its own and must still be `0`.
    #[test]
    fn a_typed_key_is_read_by_its_character() {
        assert_eq!(key_of_press(82, Some("0"), Some("0")), Some(Key::Num0));
        assert_eq!(key_of_press(49, Some(" "), Some(" ")), Some(Key::Space));
    }

    /// `/` is Shift+7 on Nordic layouts, and both characters keep Shift.
    #[test]
    fn shift_typed_punctuation_is_the_punctuation() {
        assert_eq!(key_of_press(26, Some("/"), Some("/")), Some(Key::Slash));
    }

    /// `⌥8` types `[` on Nordic layouts: the character ignoring modifiers
    /// drops ⌥ and would say `8`.
    #[test]
    fn what_option_typed_comes_before_the_key_under_it() {
        assert_eq!(
            key_of_press(28, Some("["), Some("8")),
            Some(Key::OpenBracket)
        );
    }

    /// `⌥S` types `ß` on a US layout, and Control a control code.
    #[test]
    fn a_character_with_no_key_falls_back_to_the_key_under_it() {
        assert_eq!(key_of_press(1, Some("ß"), Some("s")), Some(Key::S));
        assert_eq!(key_of_press(1, Some("\u{13}"), Some("s")), Some(Key::S));
    }

    #[test]
    fn a_press_with_no_readable_character_has_no_key() {
        assert_eq!(key_of_press(50, Some("§"), Some("§")), None);
        assert_eq!(key_of_press(50, None, None), None);
    }

    /// Shift+V and a bare `s` belong to the plugin: only the project chords
    /// go ahead of it.
    #[test]
    fn shift_v_and_a_bare_s_are_not_project_chords() {
        let press = |key, shift, command| {
            Chord::from_press(
                key,
                KeyModifiers {
                    shift,
                    command,
                    ..KeyModifiers::default()
                },
            )
        };
        assert!(PROJECT_CHORDS.contains(&press(Key::S, false, true)));
        assert!(PROJECT_CHORDS.contains(&press(Key::S, true, true)));
        assert!(PROJECT_CHORDS.contains(&press(Key::N, false, true)));
        assert!(!PROJECT_CHORDS.contains(&press(Key::S, false, false)));
        assert!(!PROJECT_CHORDS.contains(&press(Key::V, true, false)));
    }
}
