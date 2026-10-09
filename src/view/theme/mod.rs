//! The colour theme system: a `static AtomicU8` selecting the active palette,
//! and a free-function accessor per colour role.
//!
//! Every renderer calls these (`theme::bg()`, …, and `theme::track_color` by
//! colour slot through `Display::track_color`) rather than hard-coding a
//! colour, so switching theme in the settings modal recolours the whole UI with no
//! other state. The chosen index is persisted
//! (`SettingsData.theme_index`) and re-applied at startup. Layout / font-size
//! constants live at the bottom. See `030-ui-design.md`.
//!
//! The 30 palettes themselves and the `ThemePalette` shape are in `palettes.rs`.

mod palettes;

use std::sync::atomic::{AtomicU8, Ordering};

use egui::Color32;

use palettes::{THEMES, ThemePalette};

/// Index into `THEMES` of the active palette. Process-wide; read on every
/// colour lookup.
static ACTIVE_THEME: AtomicU8 = AtomicU8::new(29);

/// The active [`ThemePalette`] (modular index, so a stale value can't panic).
fn active_palette() -> &'static ThemePalette {
    &THEMES[active_theme_index()]
}

/// Number of available themes.
pub(crate) fn theme_count() -> usize {
    THEMES.len()
}

/// Name of theme `idx` (empty string if out of range).
pub(crate) fn theme_name(idx: usize) -> &'static str {
    THEMES.get(idx).map(|t| t.name).unwrap_or("")
}

/// Index of the currently active theme.
pub(crate) fn active_theme_index() -> usize {
    ACTIVE_THEME.load(Ordering::Relaxed) as usize % THEMES.len()
}

/// Makes theme `idx` the active one — the saved theme at startup, a pick in
/// the settings modal. Modular so a stale saved index (e.g. after `THEMES`
/// shrinks in a later edit) degrades to a valid theme instead of panicking.
pub(crate) fn set_active_theme(idx: usize) {
    ACTIVE_THEME.store((idx % THEMES.len()) as u8, Ordering::Relaxed);
}

/// The active theme's `bg` colour.
pub(crate) fn bg() -> Color32 {
    active_palette().bg
}

/// The active theme's `bg_panel` colour.
pub(crate) fn bg_panel() -> Color32 {
    active_palette().bg_panel
}

/// The active theme's `bg_timeline` colour.
pub(crate) fn bg_timeline() -> Color32 {
    active_palette().bg_timeline
}

/// The active theme's `separator` colour.
pub(crate) fn separator() -> Color32 {
    active_palette().separator
}

/// The active theme's `grid_major` colour.
pub(crate) fn grid_major() -> Color32 {
    active_palette().grid_major
}

/// The active theme's `fg` colour.
pub(crate) fn fg() -> Color32 {
    active_palette().fg
}

/// The active theme's `fg_dim` colour.
pub(crate) fn fg_dim() -> Color32 {
    active_palette().fg_dim
}

/// The active theme's `accent` colour.
pub(crate) fn accent() -> Color32 {
    active_palette().accent
}

/// The active theme's `playhead` colour.
pub(crate) fn playhead() -> Color32 {
    active_palette().playhead
}

/// The active theme's `region` colour.
pub(crate) fn region() -> Color32 {
    active_palette().region
}

/// The active theme's `piano_key_white` colour.
pub(crate) fn piano_key_white() -> Color32 {
    active_palette().piano_key_white
}

/// The active theme's `piano_key_black` colour.
pub(crate) fn piano_key_black() -> Color32 {
    active_palette().piano_key_black
}

/// Fill for a lit **S** (solo) track-header button.
pub(crate) fn track_solo() -> Color32 {
    active_palette().track_solo
}

/// Fill for a lit **M** (mute) track-header button.
pub(crate) fn track_mute() -> Color32 {
    active_palette().track_mute
}

// --- Track accent colors ---
/// The active theme's track colour number `color_slot` (`Track::color_slot`),
/// cycling past the palette's last.
pub(crate) fn track_color(color_slot: usize) -> Color32 {
    let tracks = &active_palette().tracks;
    tracks[color_slot % tracks.len()]
}

// --- Layout ---
/// Header bar height, pixels.
pub(crate) const HEADER_H: f32 = 42.0;

/// Status bar height, pixels.
pub(crate) const STATUS_H: f32 = 28.0;

// --- Font sizes ---
/// Header font size, pixels.
pub(crate) const FONT_SIZE_HEADER: f32 = 30.0;

/// Timeline font size, pixels.
pub(crate) const FONT_SIZE_TL: f32 = 12.0;

/// Small dim label font size (header chip labels, the footer message), pixels.
pub(crate) const FONT_SIZE_LABEL: f32 = 13.0;
