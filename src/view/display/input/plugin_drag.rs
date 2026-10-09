//! The plugin drag: an instrument plugin dragged out of the browser panel's
//! Plugins category onto a track. While it is over
//! a track's lane or header, that track is highlighted; the drop puts the
//! plugin on it (`Display::put_plugin_on_track`), the same as Enter does for the
//! selected track, and selects that track. See `030-ui-design.md` § Browser Panel.

use super::*;

impl Display {
    /// Starts dragging `plugin` out of the browser, aimed at the pointer —
    /// the same start as the `.mid` drag's (`begin_midi_drag`).
    pub(super) fn begin_plugin_drag(&mut self, plugin: BrowserPlugin) {
        self.gesture.plugin_drag = Some(PluginDrag {
            plugin,
            target: None,
        });
        if let Some(pos) = self.canvas_pointer() {
            self.update_plugin_drag_target(pos.x, pos.y);
        }
    }

    /// Re-aims a live plugin drag at pointer `(x, y)`.
    pub(super) fn update_plugin_drag_target(&mut self, x: f32, y: f32) {
        if self.gesture.plugin_drag.is_none() {
            return;
        }
        let target = self.plugin_drop_target(x, y);
        if let Some(drag) = &mut self.gesture.plugin_drag {
            drag.target = target;
        }
    }

    /// The track a plugin dropped at `(x, y)` goes on: the arranger track
    /// under it, header or lane. `None` anywhere else (the docked clip pane,
    /// the timeline strip, the performance lane, the browser).
    fn plugin_drop_target(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.pane_at(x, y) != Some(Pane::Arranger) {
            return None;
        }
        // Above the first track (the strip, the performance lane) is none.
        self.in_pane(Pane::Arranger, |display| display.track_idx_at(y))
    }

    /// The drop: selects the track under the pointer (the cursor stays put)
    /// and puts the plugin on it, so the live keyboard plays the plugin just
    /// dropped. Nothing off the tracks.
    pub(super) fn finish_plugin_drag(&mut self) {
        if let Some(PluginDrag {
            plugin,
            target: Some(track_idx),
        }) = self.gesture.plugin_drag.take()
        {
            self.input_event_tx
                .send(InputEvent::SelectTrack { track_idx })
                .ok();
            self.put_plugin_on_track(track_idx, &plugin);
        }
    }
}
