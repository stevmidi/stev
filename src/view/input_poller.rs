//! [`InputPoller`] — turns egui's per-frame native input into a flat
//! [`InputEvent`] list `Display` drains each frame.
//!
//! It derives move / press / release edges from egui's pointer *state*, passes
//! key presses through (egui delivers those as events), and translates the
//! `Copy`/`Cut`/`Paste` clipboard events — which egui-winit swallows the raw
//! key press for — into bare intents. See `input_event.rs`. It also drives its
//! own auto-repeat for the four arrow keys (DAS/ARR — an initial delay, then a
//! steady repeat rate — see [`KeyRepeat`]) rather than relying on the OS's
//! native key-repeat events, so the feel is tunable and doesn't fire for keys
//! that shouldn't auto-repeat (`Enter`, `R`, etc.); egui's own `repeat: true`
//! events are ignored, except for unmodified `+`/`=`/`-` (the arranger zoom
//! keys), which ride the OS repeat — see the comment at that arm for why.
//!
//! Files dragged in from the file manager become enter / leave / drop edges
//! (`FileDragEntered` / `FileDragLeft` / `FileDropped`). The pointer keeps
//! moving during such a drag because `Display::raw_input_hook` feeds egui the
//! OS's pointer (the windowing layer reports none), so `MouseMoved` flows and
//! every hover follows the drag as it would a plain move.

use std::path::{Path, PathBuf};

use egui::{Context, Event, HoveredFile, InputOptions, Key, PointerButton, Pos2, Vec2, vec2};

use crate::core::{
    input_event::{InputEvent, KeyModifiers},
    project::is_midi_file,
};

/// Arrow keys that auto-repeat while held. Anything not in this list fires
/// exactly once per physical press, however long it's held.
const REPEATABLE_KEYS: [Key; 4] = [
    Key::ArrowLeft,
    Key::ArrowRight,
    Key::ArrowUp,
    Key::ArrowDown,
];

/// Delay after the initial press before auto-repeat kicks in ("DAS" —
/// delayed auto shift), seconds.
const REPEAT_DELAY_SECS: f64 = 0.35;

/// Interval between repeats once auto-repeat is active ("ARR" — auto-repeat
/// rate), seconds.
const REPEAT_INTERVAL_SECS: f64 = 0.06;

/// Auto-repeat timing for one held key — when its next repeat should fire.
/// Pure and `egui`-independent so it's unit-testable without a live
/// `egui::Context`; [`InputPoller::poll`] is the only caller.
struct KeyRepeat {
    /// `egui::InputState::time` at which the next repeat should fire.
    next_fire_at: f64,
}

impl KeyRepeat {
    /// Starts the delay countdown from a fresh press at `now`.
    fn started(now: f64) -> Self {
        Self {
            next_fire_at: now + REPEAT_DELAY_SECS,
        }
    }

    /// Call once per frame while the key is held. Returns `true` (and
    /// reschedules relative to `now`, not the missed deadline, so a stalled
    /// frame can't cause a burst of catch-up repeats) when a repeat should
    /// fire this frame.
    fn poll(&mut self, now: f64) -> bool {
        if now >= self.next_fire_at {
            self.next_fire_at = now + REPEAT_INTERVAL_SECS;
            true
        } else {
            false
        }
    }
}

/// Double-click detection on the *second press*. egui's own
/// `button_double_clicked` fires on the second release, so a double-click
/// action would wait for the button to come up. Uses egui's own click limits
/// (`InputOptions::max_double_click_delay`, `max_click_dist`), measured
/// slightly differently: the delay runs from the first click's release to the
/// second *press* (egui: release to release), and a long hold still counts as
/// a click (egui drops one past `max_click_duration`). Pure, like
/// [`KeyRepeat`]; [`InputPoller::poll`] feeds it the press and release edges.
#[derive(Default)]
struct DoubleClick {
    /// Where the button went down, while it is down — `None` for the second
    /// press of a double-click, whose release is no new click.
    press_pos: Option<Pos2>,
    /// When and where the last click (a press released without dragging)
    /// ended, until a press uses it or it goes stale.
    last_click: Option<(f64, Pos2)>,
}

impl DoubleClick {
    /// A press edge at `now`, `pos`. Returns `true` when it is the second
    /// press of a double-click — within `limits.max_double_click_delay` of a
    /// click, within `limits.max_click_dist` of where it ended. A
    /// double-click uses up both its clicks, so a quick third press starts
    /// over.
    fn pressed(&mut self, now: f64, pos: Pos2, limits: &InputOptions) -> bool {
        let double = self.last_click.take().is_some_and(|(at, click_pos)| {
            now - at < limits.max_double_click_delay
                && click_pos.distance(pos) < limits.max_click_dist
        });
        self.press_pos = (!double).then_some(pos);
        double
    }

    /// A release edge at `now`, `pos`: a click if the pointer stayed within
    /// `limits.max_click_dist` of the press (and the press wasn't a
    /// double-click's second).
    fn released(&mut self, now: f64, pos: Pos2, limits: &InputOptions) {
        self.last_click = self
            .press_pos
            .take()
            .filter(|press_pos| press_pos.distance(pos) < limits.max_click_dist)
            .map(|_| (now, pos));
    }
}

/// Per-frame input translator. See the module docs.
pub(super) struct InputPoller {
    /// This frame's events, rebuilt by [`poll`](Self::poll).
    input_events: Vec<InputEvent>,
    /// Pointer position last frame, to detect a move.
    last_mouse_pos: Option<Pos2>,
    /// Primary button state last frame, to detect press / release edges.
    left_mouse_down: bool,
    /// Primary-button double-click detection, on the second press.
    double_click: DoubleClick,
    /// Auto-repeat state, index-parallel with [`REPEATABLE_KEYS`]; `None`
    /// while that key is up.
    key_repeat: [Option<KeyRepeat>; REPEATABLE_KEYS.len()],
    /// ⌘/Ctrl state last frame, to detect a release edge
    /// (`InputEvent::MoveModifierReleased`) the same way `left_mouse_down`
    /// detects `MouseReleased`.
    move_modifier_down: bool,
    /// Whether files from the file manager hovered last frame, to detect the
    /// `FileDragEntered` / `FileDragLeft` edges.
    files_hovering: bool,
}

/// The file a file-manager drag is about: the first MIDI file among `paths`,
/// else the first one.
fn drag_subject<'a>(paths: impl Iterator<Item = &'a Path> + Clone) -> Option<PathBuf> {
    paths
        .clone()
        .find(|path| is_midi_file(path))
        .or_else(|| paths.clone().next())
        .map(Path::to_path_buf)
}

/// The file-drag edge this frame, if any: a drop wins (egui clears the
/// hover in the same frame), then a hover starting, then one ending.
/// `hovered` names the hovering file; only called on the frame a hover
/// starts, so a steady hover allocates nothing.
fn file_drag_edge(
    was_hovering: bool,
    hovering: bool,
    hovered: impl FnOnce() -> Option<PathBuf>,
    dropped: Option<PathBuf>,
) -> Option<InputEvent> {
    if let Some(path) = dropped {
        return Some(InputEvent::FileDropped { path });
    }
    match (was_hovering, hovering) {
        (false, true) => hovered().map(|path| InputEvent::FileDragEntered { path }),
        (true, false) => Some(InputEvent::FileDragLeft),
        _ => None,
    }
}

impl InputPoller {
    /// A fresh poller with no events.
    pub(super) fn new() -> Self {
        Self {
            input_events: Vec::new(),
            last_mouse_pos: None,
            left_mouse_down: false,
            double_click: DoubleClick::default(),
            key_repeat: [const { None }; REPEATABLE_KEYS.len()],
            move_modifier_down: false,
            files_hovering: false,
        }
    }

    /// Takes this frame's events, leaving the list empty until the next
    /// [`poll`](Self::poll). Hand the drained list back with
    /// [`recycle_input_events`](Self::recycle_input_events) so the next frame
    /// reuses its allocation.
    pub(super) fn take_input_events(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.input_events)
    }

    /// The pointer in canvas space as the last [`poll`](Self::poll) saw it,
    /// `None` with no pointer over the window.
    pub(super) fn pointer_pos(&self) -> Option<Pos2> {
        self.last_mouse_pos
    }

    /// Returns a list taken with [`take_input_events`](Self::take_input_events)
    /// for the next [`poll`](Self::poll) to refill.
    pub(super) fn recycle_input_events(&mut self, mut events: Vec<InputEvent>) {
        events.clear();
        self.input_events = events;
    }

    /// Rebuilds the event list from `ctx`'s current-frame input. Pointer
    /// positions come out in canvas space: shifted left by `canvas_left`, the
    /// browser panel's width while it shows (the canvas is drawn that far
    /// right), so a position over the panel has a negative x.
    pub(super) fn poll(&mut self, ctx: &Context, canvas_left: f32) {
        self.input_events.clear();
        // Read before `ctx.input`, which holds the context lock.
        let click_limits = ctx.options(|options| options.input_options);

        ctx.input(|i| {
            let modifiers = KeyModifiers::from(i.modifiers);
            let latest_pos = i
                .pointer
                .latest_pos()
                .map(|pos| pos - vec2(canvas_left, 0.0));

            // Mouse move
            let pos = latest_pos.unwrap_or_default();
            if let Some(last) = self.last_mouse_pos
                && pos != last
            {
                self.input_events.push(InputEvent::MouseMoved {
                    x: pos.x,
                    y: pos.y,
                    modifiers,
                });
            }
            self.last_mouse_pos = latest_pos;

            // Files from the file manager: after the move above, so a drop
            // lands where the pointer was last seen.
            let hovering = !i.raw.hovered_files.is_empty();
            let hovered = || {
                drag_subject(
                    i.raw
                        .hovered_files
                        .iter()
                        .filter_map(|file: &HoveredFile| file.path.as_deref()),
                )
            };
            let dropped = drag_subject(i.raw.dropped_files.iter().map(|file| file.path()));
            if let Some(edge) = file_drag_edge(self.files_hovering, hovering, hovered, dropped) {
                self.input_events.push(edge);
            }
            self.files_hovering = hovering;

            // Mouse click (press edge) / release edge
            let left_down = i.pointer.button_down(PointerButton::Primary);
            // The second press of a double-click is still a press: its
            // `MouseClicked` goes first, then `MouseDoubleClicked`.
            if left_down && !self.left_mouse_down {
                self.input_events.push(InputEvent::MouseClicked {
                    x: pos.x,
                    y: pos.y,
                    modifiers,
                });
                if self.double_click.pressed(i.time, pos, &click_limits) {
                    self.input_events
                        .push(InputEvent::MouseDoubleClicked { x: pos.x, y: pos.y });
                }
            } else if !left_down && self.left_mouse_down {
                self.input_events.push(InputEvent::MouseReleased);
                self.double_click.released(i.time, pos, &click_limits);
            }
            self.left_mouse_down = left_down;

            // Raw motion with the button held, which (unlike the position
            // above) keeps coming past the window and screen edges.
            if left_down
                && let Some(motion) = i.pointer.motion()
                && motion != Vec2::ZERO
            {
                self.input_events.push(InputEvent::PointerMotion {
                    dx: motion.x,
                    dy: motion.y,
                });
            }

            // ⌘/Ctrl release edge — the commit trigger for the `⌘/Ctrl+←`/`→`
            // marquee nudge. Polled from live modifier state rather than an
            // `egui::Event`, the same way the auto-repeat loop below polls
            // `key_down` rather than relying on press/release events.
            // `modifiers.command` is ⌘ on macOS and Ctrl elsewhere — the same
            // key every `⌘/Ctrl+X` binding in the app tests.
            let move_modifier_down = i.modifiers.command;
            if !move_modifier_down && self.move_modifier_down {
                self.input_events.push(InputEvent::MoveModifierReleased);
            }
            self.move_modifier_down = move_modifier_down;

            // Key presses — egui delivers these as events, not state
            for event in &i.events {
                match event {
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers: mods,
                        ..
                    } => {
                        self.input_events.push(InputEvent::KeyPressed {
                            key: *key,
                            modifiers: KeyModifiers::from(*mods),
                        });
                    }
                    // The zoom / adjustment keys repeat on the OS's own
                    // key-repeat rather than `KeyRepeat` below: egui's
                    // `key_down` tracks the *logical* key, so `+` held as
                    // Shift+`=` and released after Shift reports its release
                    // as `=` and would leave `+` down — a state-driven repeat
                    // would then zoom forever. OS repeat events stop with the
                    // physical key. Unmodified only, so a held
                    // `⌥=`/`⌥-` (clip stretch)
                    // still fires once per press.
                    Event::Key {
                        key: key @ (Key::Plus | Key::Equals | Key::Minus),
                        pressed: true,
                        repeat: true,
                        modifiers: mods,
                        ..
                    } if !mods.shift && !mods.command && !mods.alt => {
                        self.input_events.push(InputEvent::KeyPressed {
                            key: *key,
                            modifiers: KeyModifiers::default(),
                        });
                    }
                    Event::Copy => {
                        self.input_events.push(InputEvent::Copy {
                            shift: i.modifiers.shift,
                        });
                    }
                    Event::Cut => {
                        self.input_events.push(InputEvent::Cut {
                            shift: i.modifiers.shift,
                        });
                    }
                    // egui-winit only emits `Paste` when the OS clipboard holds
                    // non-empty text, so a clip-copy first primes it with
                    // `CLIP_CLIPBOARD_SENTINEL`; the text itself is never read.
                    Event::Paste(_) => {
                        self.input_events.push(InputEvent::Paste);
                    }
                    _ => {}
                }
            }

            // Auto-repeat: a held arrow key re-fires at `REPEAT_INTERVAL_SECS`
            // once `REPEAT_DELAY_SECS` has elapsed since the press. Driven off
            // `key_down` state rather than the press/release edges above, so a
            // release event lost to e.g. a focus change still self-corrects
            // next frame instead of wedging into a stuck repeat; the same
            // branch that clears a released key's state also lazily starts the
            // delay countdown for one whose press edge was never seen.
            for (idx, key) in REPEATABLE_KEYS.iter().enumerate() {
                if !i.key_down(*key) {
                    self.key_repeat[idx] = None;
                    continue;
                }
                let fire = match &mut self.key_repeat[idx] {
                    Some(state) => state.poll(i.time),
                    None => {
                        self.key_repeat[idx] = Some(KeyRepeat::started(i.time));
                        false
                    }
                };
                if fire {
                    self.input_events.push(InputEvent::KeyPressed {
                        key: *key,
                        modifiers,
                    });
                }
            }

            // Two-finger trackpad / wheel scroll. egui accumulates and smooths
            // this per frame; with no `ScrollArea` anywhere nothing else
            // consumes it. X scrolls time; Y scrolls the piano roll's pitch
            // or the arranger's track lanes (once they no longer fit).
            // `smooth_scroll_delta` already respects the OS natural-scrolling
            // setting.
            let scroll = i.smooth_scroll_delta;
            if scroll != Vec2::ZERO {
                self.input_events.push(InputEvent::TimelineScroll {
                    delta_x: scroll.x,
                    delta_y: scroll.y,
                    pointer_x: latest_pos.map(|pos| pos.x),
                    pointer_y: latest_pos.map(|pos| pos.y),
                });
            }

            // ⌘/Ctrl+wheel and trackpad pinch. egui routes a wheel delta
            // carrying its `zoom_modifier` (⌘ on macOS, Ctrl elsewhere) here
            // and leaves `smooth_scroll_delta` zero for it, so this and the
            // scroll above never both fire for one gesture.
            let zoom = i.zoom_delta();
            if zoom != 1.0 {
                self.input_events.push(InputEvent::TimelineZoom {
                    factor: zoom,
                    pointer_x: latest_pos.map(|pos| pos.x),
                    pointer_y: latest_pos.map(|pos| pos.y),
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_fire_before_the_delay_elapses() {
        let mut repeat = KeyRepeat::started(0.0);
        assert!(!repeat.poll(0.1));
        assert!(!repeat.poll(REPEAT_DELAY_SECS - 0.01));
    }

    #[test]
    fn fires_once_the_delay_elapses_then_at_the_repeat_interval() {
        let mut repeat = KeyRepeat::started(0.0);
        assert!(repeat.poll(REPEAT_DELAY_SECS));
        assert!(!repeat.poll(REPEAT_DELAY_SECS + REPEAT_INTERVAL_SECS - 0.01));
        assert!(repeat.poll(REPEAT_DELAY_SECS + REPEAT_INTERVAL_SECS));
    }

    #[test]
    fn a_stalled_frame_fires_once_and_resyncs_instead_of_bursting() {
        let mut repeat = KeyRepeat::started(0.0);
        let stalled_now = REPEAT_DELAY_SECS + 5.0;
        assert!(repeat.poll(stalled_now));
        // Rescheduled from `stalled_now`, not from the missed deadline, so
        // the backlog doesn't collapse into a burst of repeats next frame.
        assert!(!repeat.poll(stalled_now + REPEAT_INTERVAL_SECS - 0.01));
        assert!(repeat.poll(stalled_now + REPEAT_INTERVAL_SECS));
    }

    fn pos(x: f32) -> Pos2 {
        Pos2::new(x, 0.0)
    }

    /// A press at `t` and a release 0.05 s later, both at `x`. Returns
    /// whether the press was a double-click's second.
    fn click(clicks: &mut DoubleClick, t: f64, x: f32) -> bool {
        let limits = InputOptions::default();
        let double = clicks.pressed(t, pos(x), &limits);
        clicks.released(t + 0.05, pos(x), &limits);
        double
    }

    #[test]
    fn a_second_press_soon_after_a_click_is_a_double_click() {
        let mut clicks = DoubleClick::default();
        assert!(!click(&mut clicks, 0.0, 10.0));
        assert!(
            clicks.pressed(0.2, pos(12.0), &InputOptions::default()),
            "fires on the press, not the release"
        );
    }

    #[test]
    fn a_late_second_press_is_not_a_double_click() {
        let limits = InputOptions::default();
        let mut clicks = DoubleClick::default();
        click(&mut clicks, 0.0, 10.0);
        assert!(!clicks.pressed(0.05 + limits.max_double_click_delay, pos(10.0), &limits));
    }

    #[test]
    fn a_distant_second_press_is_not_a_double_click() {
        let limits = InputOptions::default();
        let mut clicks = DoubleClick::default();
        click(&mut clicks, 0.0, 10.0);
        assert!(!clicks.pressed(0.1, pos(10.0 + limits.max_click_dist), &limits));
    }

    #[test]
    fn a_drag_is_not_the_first_click_of_a_double_click() {
        let limits = InputOptions::default();
        let mut clicks = DoubleClick::default();
        clicks.pressed(0.0, pos(10.0), &limits);
        clicks.released(0.1, pos(40.0), &limits);
        assert!(!clicks.pressed(0.2, pos(40.0), &limits));
    }

    #[test]
    fn a_third_quick_press_starts_over() {
        let mut clicks = DoubleClick::default();
        assert!(!click(&mut clicks, 0.0, 10.0));
        assert!(click(&mut clicks, 0.1, 10.0));
        assert!(!click(&mut clicks, 0.2, 10.0));
        // … but that third press is a click a fourth can pair with.
        assert!(click(&mut clicks, 0.3, 10.0));
    }

    #[test]
    fn a_drag_is_about_its_first_midi_file() {
        let paths = [Path::new("a.txt"), Path::new("b.mid"), Path::new("c.mid")];
        assert_eq!(
            drag_subject(paths.iter().copied()),
            Some(PathBuf::from("b.mid"))
        );
        let none_midi = [Path::new("a.txt"), Path::new("b.wav")];
        assert_eq!(
            drag_subject(none_midi.iter().copied()),
            Some(PathBuf::from("a.txt"))
        );
        assert_eq!(drag_subject([].iter().copied()), None);
    }

    fn edge_name(edge: Option<InputEvent>) -> Option<&'static str> {
        edge.map(|event| match event {
            InputEvent::FileDragEntered { .. } => "entered",
            InputEvent::FileDragLeft => "left",
            InputEvent::FileDropped { .. } => "dropped",
            _ => "other",
        })
    }

    #[test]
    fn file_drags_report_enter_leave_and_drop_edges_once() {
        let path = || Some(PathBuf::from("a.mid"));
        let edge = |was, now, dropped| edge_name(file_drag_edge(was, now, path, dropped));
        assert_eq!(edge(false, true, None), Some("entered"));
        assert_eq!(edge(true, true, None), None);
        assert_eq!(edge(true, false, None), Some("left"));
        assert_eq!(edge(false, false, None), None);
        assert_eq!(edge(true, false, path()), Some("dropped"));
        assert_eq!(edge(false, false, path()), Some("dropped"));
    }

    #[test]
    fn a_steady_hover_never_looks_for_its_file() {
        let edge = file_drag_edge(true, true, || panic!("resolved the hover"), None);
        assert!(edge.is_none());
    }
}
