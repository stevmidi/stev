//! The MIDI clip import's drag: a `.mid` dragged from the file manager or
//! out of the browser panel onto an arranger lane. From the moment the file
//! comes over the window, the cursor line follows the pointer as on any move
//! and a ghost of the clip-to-be rides it, on the lane under the pointer at
//! the cursor line's snapped tick; the drop puts the clip there
//! (`InputEvent::ImportMidiClip`). Enter on a `.mid` in the browser imports
//! it without a drag, on the selected track at the cursor. See
//! `060-persistence.md` § MIDI clip import and `030-ui-design.md` § Browser
//! Panel.
//!
//! The windowing layer reports no pointer positions while the OS runs a file
//! drag, so on macOS [`DragPointer`] reads the pointer from AppKit and
//! `Display::raw_input_hook` hands it to egui as an ordinary pointer move
//! each frame the files hover.

use std::path::Path;

use eframe::CreationContext;
use egui::{Event, Pos2, RawInput};
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_app_kit::NSView;

use crate::core::project::{is_midi_file, load_midi_clip};
use crate::core::time::Meter;
use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::clip::Clip;
use crate::shapes::clip_shape::ClipShape;
#[cfg(target_os = "macos")]
use crate::view::appkit::content_view;

use super::*;

/// `path`'s file name without its extension, for the footer.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
}

impl MidiDrag {
    /// Reads the `.mid` at `path` for a drag, with no target yet. Anything
    /// not named like a MIDI file is refused unread — a dragged-in video
    /// isn't loaded into memory to find that out.
    fn load(path: &Path, meter: Meter) -> Self {
        let name = file_stem(path);
        let loaded = if is_midi_file(path) {
            load_midi_clip(path, meter).map(|clip| LoadedMidi {
                ghost: ClipShape::from_metadata(ClipMetadata::from_clip(0, &clip)),
                clip,
            })
        } else {
            Err("not a MIDI file".to_owned())
        };
        Self {
            name,
            loaded,
            target: None,
        }
    }

    /// The ghost and where it goes — `(shape, track index, start tick)` —
    /// while the file is readable and the pointer over a lane.
    pub(in crate::view::display) fn ghost_at(&self) -> Option<(&ClipShape, usize, i32)> {
        let (track_idx, tick) = self.target?;
        let loaded = self.loaded.as_ref().ok()?;
        Some((&loaded.ghost, track_idx, tick))
    }
}

impl Display {
    /// Starts dragging the `.mid` at `path` (a file-manager drag coming over
    /// the window, or a browser row dragged out of the panel): reads it, and
    /// aims the ghost at the pointer.
    pub(super) fn begin_midi_drag(&mut self, path: &Path) {
        self.gesture.midi_drag = Some(MidiDrag::load(path, self.meter()));
        if let Some(pos) = self.canvas_pointer() {
            self.update_midi_drag_target(pos.x, pos.y);
        }
    }

    /// Re-aims a live drag's ghost at pointer `(x, y)`.
    pub(super) fn update_midi_drag_target(&mut self, x: f32, y: f32) {
        if self.gesture.midi_drag.is_none() {
            return;
        }
        let target = self.midi_drop_target(x, y);
        if let Some(drag) = &mut self.gesture.midi_drag {
            drag.target = target;
        }
    }

    /// Where a clip dropped at `(x, y)` lands: the arranger lane under it, at
    /// the snapped tick the cursor line shows there — the same grid test a
    /// click uses (`is_mouse_inside_grid`), so the ghost appears exactly where
    /// a click would put the cursor. `None` anywhere else (the docked clip
    /// pane, the track headers, the performance lane, the browser).
    fn midi_drop_target(&mut self, x: f32, y: f32) -> Option<(usize, i32)> {
        if self.pane_at(x, y) != Some(Pane::Arranger) {
            return None;
        }
        self.in_pane(Pane::Arranger, |display| {
            if !display.is_mouse_inside_grid(x, y) || display.performance_lane_hit_at(y) {
                return None;
            }
            display
                .track_idx_at(y)
                .map(|track_idx| (track_idx, display.snapped_tick_at(x)))
        })
    }

    /// The drop: imports the clip where the ghost shows it, says why not if
    /// the file couldn't be read, and does nothing off the lanes.
    pub(super) fn finish_midi_drag(&mut self) {
        let Some(drag) = self.gesture.midi_drag.take() else {
            return;
        };
        match (drag.loaded, drag.target) {
            (Ok(loaded), Some(target)) => {
                self.send_midi_import(loaded.clip, drag.name, Some(target))
            }
            (Err(e), _) => self.report_unreadable_midi(&drag.name, &e),
            (Ok(_), None) => {}
        }
    }

    /// Enter on a `.mid` in the browser: imports it on the selected track at
    /// the cursor (the handler resolves both).
    pub(in crate::view::display) fn import_midi_file(&mut self, path: &Path) {
        let name = file_stem(path);
        match load_midi_clip(path, self.meter()) {
            Ok(clip) => self.send_midi_import(clip, name, None),
            Err(e) => self.report_unreadable_midi(&name, &e),
        }
    }

    /// Hands an imported clip to the handler (`PasteClipsEdit::importing`).
    fn send_midi_import(&self, clip: Clip, name: String, target: Option<(usize, i32)>) {
        self.input_event_tx
            .send(InputEvent::ImportMidiClip {
                clip: Box::new(clip),
                name,
                target,
            })
            .ok();
    }

    /// Says in the footer why `name` could not be imported.
    fn report_unreadable_midi(&mut self, name: &str, error: &str) {
        self.render.status = Some(StatusMessage::new(format!(
            "Could not import {name}: {error}"
        )));
    }
}

/// The window's native view, kept to read the pointer while files from the
/// file manager hover the window — winit reports none during a drag. Taken
/// from eframe's creation context (`Display::attach_drag_pointer`).
#[cfg(target_os = "macos")]
pub(crate) struct DragPointer {
    /// eframe's content view, retained for the app's lifetime.
    view: Retained<NSView>,
}

#[cfg(target_os = "macos")]
impl DragPointer {
    /// The holder for the new window's content view, or `None` without one.
    pub(crate) fn from_window(cc: &CreationContext<'_>) -> Option<Self> {
        content_view(cc).map(|view| Self { view })
    }

    /// The pointer in egui points (AppKit's points over egui's `zoom`), read
    /// with `mouseLocationOutsideOfEventStream`, which needs no event.
    fn pointer(&self, zoom: f32) -> Option<Pos2> {
        let window = self.view.window()?;
        let point = self
            .view
            .convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
        let y = if self.view.isFlipped() {
            point.y
        } else {
            self.view.bounds().size.height - point.y
        };
        Some(Pos2::new(point.x as f32 / zoom, y as f32 / zoom))
    }
}

/// Off macOS there is no native pointer read: during a file drag the pointer
/// is whatever the windowing layer reports (see `docs/240` § B's smoke tests).
#[cfg(not(target_os = "macos"))]
pub(crate) struct DragPointer;

#[cfg(not(target_os = "macos"))]
impl DragPointer {
    /// Always `None` off macOS.
    pub(crate) fn from_window(_cc: &CreationContext<'_>) -> Option<Self> {
        None
    }

    /// Never called: no holder exists off macOS.
    fn pointer(&self, _zoom: f32) -> Option<Pos2> {
        None
    }
}

impl Display {
    /// Keeps the new window's native view for
    /// [`inject_drag_pointer`](Self::inject_drag_pointer). Called once, from
    /// eframe's app-creation closure.
    pub(crate) fn attach_drag_pointer(&mut self, cc: &CreationContext<'_>) {
        self.drag_pointer = DragPointer::from_window(cc);
    }

    /// `App::raw_input_hook`: while files from the file manager hover (or
    /// land, that frame), moves egui's pointer to the OS's, so egui's own
    /// pointer — and every `MouseMoved`, hover and the import ghost — follow
    /// the drag, and stay right after the drop.
    pub(in crate::view::display) fn inject_drag_pointer(
        &self,
        ctx: &egui::Context,
        raw_input: &mut RawInput,
    ) {
        if raw_input.hovered_files.is_empty() && raw_input.dropped_files.is_empty() {
            return;
        }
        if let Some(pos) = self
            .drag_pointer
            .as_ref()
            .and_then(|pointer| pointer.pointer(ctx.zoom_factor()))
        {
            raw_input.events.push(Event::PointerMoved(pos));
        }
    }
}
