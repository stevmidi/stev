//! The track header's output menu: the small popup a click on a track's
//! output chip (`Ch 3`, or the plugin's name) opens. It routes the track back
//! to MIDI out on a picked channel — a 4×4 grid of channels 1–16 — and, on a
//! plugin track, offers `Remove <plugin>` (MIDI back on the track's own
//! channel); `Rename` and `Delete track` at the bottom rename the track (as
//! ⌘R) and remove it (undoable, as Delete in track header focus) — `Delete
//! track` absent on the last track, which can't be deleted. Mouse-only: a
//! click outside or any key closes it. Putting a plugin *on* a track is the
//! browser panel's job, not this menu's.
//!
//! The layout is a pure function of the chip's rect ([`output_menu_layout`],
//! unit-tested) shared by drawing (`rendering/output_menu.rs`) and the click
//! hit-test here, so the two can't drift. See `030-ui-design.md` § Arranger
//! Track Header.

use egui::{Key, Pos2, Rect, pos2, vec2};

use crate::core::input_event::InputEvent;
use crate::models::track::TrackOutput;

use super::status_message::StatusMessage;
use super::{Display, Pane, TrackRoute};

/// Inner margin around the menu's rows.
const MENU_PAD: f32 = 4.0;
/// Height of one menu row (the plugin's name, `Remove`, the channel label, a
/// row of the channel grid, `Rename`, `Delete track`).
const MENU_ROW_H: f32 = 18.0;
/// Width of one channel-grid cell.
const CHANNEL_CELL_W: f32 = 26.0;
/// Channel-grid columns (and rows: 16 channels in a 4×4 grid).
const CHANNEL_COLS: usize = 4;
/// Height of the divider band between the plugin rows and the channel grid,
/// and between the grid and `Rename`.
const MENU_DIVIDER_H: f32 = 7.0;
/// Gap between the chip and the menu.
const MENU_GAP: f32 = 2.0;

/// Where each part of the output menu sits on screen.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OutputMenuLayout {
    /// The whole menu.
    pub(super) panel: Rect,
    /// The plugin's name and format — a plugin track only.
    pub(super) title: Option<Rect>,
    /// `Remove <plugin>` — a plugin track only.
    pub(super) remove: Option<Rect>,
    /// The y of the divider line under `Remove` — a plugin track only.
    pub(super) divider_y: Option<f32>,
    /// The "MIDI channel" label above the grid.
    pub(super) label: Rect,
    /// The channel cells, channel 0 (shown as 1) first, row by row.
    pub(super) channels: [Rect; 16],
    /// The y of the divider line between the grid and `Rename`.
    pub(super) track_divider_y: f32,
    /// `Rename`, under the grid.
    pub(super) rename: Rect,
    /// `Delete track`, the bottom row — `None` on the last track.
    pub(super) delete: Option<Rect>,
}

/// What a click in the output menu picks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum OutputMenuHit {
    /// `Remove <plugin>`.
    Remove,
    /// MIDI out on this channel (0–15).
    Channel(u8),
    /// `Rename`.
    Rename,
    /// `Delete track`.
    DeleteTrack,
}

impl OutputMenuLayout {
    /// The item under `p`, if any — `None` over the title, the label, the
    /// padding or outside the menu.
    pub(super) fn hit_at(&self, p: Pos2) -> Option<OutputMenuHit> {
        if self.remove.is_some_and(|r| r.contains(p)) {
            return Some(OutputMenuHit::Remove);
        }
        if self.rename.contains(p) {
            return Some(OutputMenuHit::Rename);
        }
        if self.delete.is_some_and(|r| r.contains(p)) {
            return Some(OutputMenuHit::DeleteTrack);
        }
        self.channels
            .iter()
            .position(|cell| cell.contains(p))
            .map(|idx| OutputMenuHit::Channel(idx as u8))
    }
}

/// Lays the output menu out under `chip` (left-aligned with it), or above it
/// when it would run past `bounds`' bottom edge, and pulled left to stay
/// inside `bounds`. `plugin` adds the plugin's name and `Remove` rows above
/// the channel grid; under a divider below the grid come `Rename` and, when
/// `deletable` (not on the last track), `Delete track`. Pure geometry,
/// unit-tested.
pub(super) fn output_menu_layout(
    chip: Rect,
    plugin: bool,
    deletable: bool,
    bounds: Rect,
) -> OutputMenuLayout {
    let width = 2.0 * MENU_PAD + CHANNEL_COLS as f32 * CHANNEL_CELL_W;
    let plugin_h = if plugin {
        2.0 * MENU_ROW_H + MENU_DIVIDER_H
    } else {
        0.0
    };
    let grid_rows = 16 / CHANNEL_COLS;
    let track_rows = if deletable { 2 } else { 1 };
    let track_h = MENU_DIVIDER_H + track_rows as f32 * MENU_ROW_H;
    let height = 2.0 * MENU_PAD + plugin_h + (1 + grid_rows) as f32 * MENU_ROW_H + track_h;

    let below = chip.max.y + MENU_GAP;
    let top = if below + height > bounds.max.y {
        chip.min.y - MENU_GAP - height
    } else {
        below
    };
    let left = chip.min.x.min(bounds.max.x - width);
    let panel = Rect::from_min_size(pos2(left, top), vec2(width, height));

    let inner_left = left + MENU_PAD;
    let inner_w = width - 2.0 * MENU_PAD;
    let row = |y: f32| Rect::from_min_size(pos2(inner_left, y), vec2(inner_w, MENU_ROW_H));
    let mut y = top + MENU_PAD;
    let (title, remove, divider_y) = if plugin {
        let rows = (
            Some(row(y)),
            Some(row(y + MENU_ROW_H)),
            Some(y + 2.0 * MENU_ROW_H + MENU_DIVIDER_H * 0.5),
        );
        y += plugin_h;
        rows
    } else {
        (None, None, None)
    };
    let label = row(y);
    let grid_top = y + MENU_ROW_H;
    let channels = std::array::from_fn(|idx| {
        let (col, grid_row) = (idx % CHANNEL_COLS, idx / CHANNEL_COLS);
        Rect::from_min_size(
            pos2(
                inner_left + col as f32 * CHANNEL_CELL_W,
                grid_top + grid_row as f32 * MENU_ROW_H,
            ),
            vec2(CHANNEL_CELL_W, MENU_ROW_H),
        )
    });

    let grid_bottom = grid_top + grid_rows as f32 * MENU_ROW_H;
    let track_divider_y = grid_bottom + MENU_DIVIDER_H * 0.5;
    let rename = row(grid_bottom + MENU_DIVIDER_H);
    let delete = deletable.then(|| row(rename.max.y));

    OutputMenuLayout {
        panel,
        title,
        remove,
        divider_y,
        label,
        channels,
        track_divider_y,
        rename,
        delete,
    }
}

impl Display {
    /// The open output menu's track and layout — `None` when it is closed,
    /// or the arranger (where its chip lives) isn't on screen.
    pub(super) fn output_menu(&mut self) -> Option<(usize, OutputMenuLayout)> {
        let track = self.gesture.output_menu?;
        if !self.is_pane_visible(Pane::Arranger) {
            return None;
        }
        let chip = self
            .in_pane(Pane::Arranger, |display| display.track_header_rects(track))?
            .output;
        let plugin = matches!(self.tracks[track].route, TrackRoute::Instrument { .. });
        Some((
            track,
            output_menu_layout(
                chip,
                plugin,
                self.track_count() > 1,
                self.render.canvas_rect,
            ),
        ))
    }

    /// Opens `track`'s output menu (a click on its chip).
    pub(super) fn open_output_menu(&mut self, track: usize) {
        self.gesture.output_menu = Some(track);
    }

    /// Closes the output menu, if it is open.
    pub(super) fn close_output_menu(&mut self) {
        self.gesture.output_menu = None;
    }

    /// Whether the pointer is on one of the open menu's items — for the
    /// pointing-hand cursor.
    pub(super) fn is_pointer_on_output_menu_item(&mut self) -> bool {
        let pointer = self.canvas_pointer();
        self.output_menu()
            .zip(pointer)
            .is_some_and(|((_, layout), p)| layout.hit_at(p).is_some())
    }

    /// While the output menu is open it gets first look at every event:
    /// it takes every press (`click_output_menu`), and any key closes it —
    /// Esc then does nothing else, every other key goes on to act as usual.
    /// Returns whether the event was consumed.
    pub(super) fn handle_output_menu_input_event(&mut self, event: &InputEvent) -> bool {
        if self.gesture.output_menu.is_none() {
            return false;
        }
        match *event {
            InputEvent::MouseClicked { x, y, .. } => {
                self.click_output_menu(x, y);
                true
            }
            InputEvent::KeyPressed { key, .. } => {
                self.close_output_menu();
                key == Key::Escape
            }
            _ => false,
        }
    }

    /// A press while the output menu is open: an item acts and closes it,
    /// the rest of the menu does nothing, anywhere else just closes it (the
    /// press doesn't reach what's under it — the click-away convention).
    fn click_output_menu(&mut self, x: f32, y: f32) {
        let Some((track, layout)) = self.output_menu() else {
            self.close_output_menu();
            return;
        };
        let p = pos2(x, y);
        match layout.hit_at(p) {
            Some(OutputMenuHit::Remove) => self.route_track_to_midi(track, track as u8),
            Some(OutputMenuHit::Channel(channel)) => self.route_track_to_midi(track, channel),
            Some(OutputMenuHit::Rename) => self.open_track_rename(track),
            Some(OutputMenuHit::DeleteTrack) => {
                self.input_event_tx
                    .send(InputEvent::RemoveTrack {
                        track_idx: Some(track),
                    })
                    .ok();
            }
            None if layout.panel.contains(p) => return,
            None => {}
        }
        self.close_output_menu();
    }

    /// Routes `track` to MIDI out on `channel`, removing its plugin if it has
    /// one — not undoable, like replacing a plugin; the footer says so. A
    /// no-op when the track is already there.
    fn route_track_to_midi(&mut self, track: usize, channel: u8) {
        let removed = match &self.tracks[track].route {
            TrackRoute::MidiOut { channel: current } if *current == channel => return,
            TrackRoute::MidiOut { .. } => None,
            TrackRoute::Instrument { name } => Some(name.clone()),
        };
        #[cfg(target_os = "macos")]
        self.remove_track_instrument(track);
        self.input_event_tx
            .send(InputEvent::SetTrackOutput {
                track,
                output: TrackOutput::MidiOut { channel },
            })
            .ok();
        if let Some(name) = removed {
            let message = format!(
                "Removed {name} from track {}, MIDI channel {}",
                track + 1,
                channel + 1
            );
            self.render.status = Some(StatusMessage::new(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};

    use super::{CHANNEL_CELL_W, MENU_PAD, OutputMenuHit, output_menu_layout};

    /// A chip like a header's, in a tall window.
    fn chip() -> Rect {
        Rect::from_min_max(pos2(83.0, 200.0), pos2(112.0, 214.0))
    }

    /// A window big enough for the menu to open below the chip.
    fn bounds() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 800.0))
    }

    #[test]
    fn opens_below_the_chip_left_aligned_with_it() {
        let menu = output_menu_layout(chip(), false, true, bounds());
        assert!(menu.panel.min.y > chip().max.y);
        assert_eq!(menu.panel.min.x, chip().min.x);
        assert!(bounds().contains_rect(menu.panel));
    }

    #[test]
    fn flips_above_the_chip_near_the_bottom_edge() {
        let low = chip().translate(vec2(0.0, 560.0));
        let menu = output_menu_layout(low, true, true, bounds());
        assert!(menu.panel.max.y < low.min.y);
    }

    #[test]
    fn pulled_left_to_stay_inside_a_narrow_window() {
        let narrow = Rect::from_min_max(pos2(0.0, 0.0), pos2(150.0, 800.0));
        let menu = output_menu_layout(chip(), false, true, narrow);
        assert_eq!(menu.panel.max.x, 150.0);
    }

    #[test]
    fn a_plugin_track_adds_name_and_remove_rows_above_the_grid() {
        let midi = output_menu_layout(chip(), false, true, bounds());
        assert!(midi.title.is_none() && midi.remove.is_none());

        let plugin = output_menu_layout(chip(), true, true, bounds());
        let (title, remove) = (plugin.title.unwrap(), plugin.remove.unwrap());
        assert!(title.max.y <= remove.min.y);
        let divider_y = plugin.divider_y.unwrap();
        assert!(remove.max.y < divider_y && divider_y < plugin.label.min.y);
        assert!(midi.divider_y.is_none());
        assert!(remove.max.y < plugin.label.min.y);
        assert!(plugin.label.max.y <= plugin.channels[0].min.y);
        assert!(plugin.panel.height() > midi.panel.height());
        assert!(plugin.panel.contains_rect(plugin.channels[15]));
    }

    #[test]
    fn rename_then_delete_track_close_the_menu_under_a_divider() {
        for plugin in [false, true] {
            let menu = output_menu_layout(chip(), plugin, true, bounds());
            let (rename, delete) = (menu.rename, menu.delete.unwrap());
            assert!(menu.channels[15].max.y < menu.track_divider_y);
            assert!(menu.track_divider_y < rename.min.y);
            assert_eq!(rename.max.y, delete.min.y);
            assert_eq!(delete.max.y, menu.panel.max.y - MENU_PAD);
        }
    }

    #[test]
    fn the_last_track_has_rename_but_no_delete_row() {
        let menu = output_menu_layout(chip(), false, false, bounds());
        assert!(menu.delete.is_none());
        assert_eq!(menu.rename.max.y, menu.panel.max.y - MENU_PAD);
    }

    #[test]
    fn channels_run_row_by_row_in_a_four_by_four_grid() {
        let menu = output_menu_layout(chip(), false, true, bounds());
        let c = &menu.channels;
        assert_eq!(c[0].min.x, menu.panel.min.x + MENU_PAD);
        assert_eq!(c[1].min.x, c[0].min.x + CHANNEL_CELL_W);
        assert_eq!(c[1].min.y, c[0].min.y);
        assert_eq!(c[4].min.x, c[0].min.x);
        assert!(c[4].min.y > c[0].min.y);
        assert_eq!(c[15].max.x, menu.panel.max.x - MENU_PAD);
    }

    #[test]
    fn hits_name_each_item_and_nothing_else() {
        let menu = output_menu_layout(chip(), true, true, bounds());
        assert_eq!(
            menu.hit_at(menu.remove.unwrap().center()),
            Some(OutputMenuHit::Remove)
        );
        for (idx, cell) in menu.channels.iter().enumerate() {
            assert_eq!(
                menu.hit_at(cell.center()),
                Some(OutputMenuHit::Channel(idx as u8))
            );
        }
        assert_eq!(
            menu.hit_at(menu.rename.center()),
            Some(OutputMenuHit::Rename)
        );
        assert_eq!(
            menu.hit_at(menu.delete.unwrap().center()),
            Some(OutputMenuHit::DeleteTrack)
        );
        assert_eq!(menu.hit_at(menu.title.unwrap().center()), None);
        assert_eq!(menu.hit_at(menu.label.center()), None);
        assert_eq!(menu.hit_at(chip().center()), None);
    }
}
