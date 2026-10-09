//! App-wide settings (not project data): the remembered MIDI ports, the
//! MIDI-out offset, the last project folder and the theme index. Serialized to
//! `settings.json` in the platform config directory (`paths::config_dir`).
//! Like the project DTOs, a new field must be `#[serde(default)]` — but here
//! the default is often *not* the zero value, so an upgrading user gets
//! current behaviour.

use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::config::{MIDI_OUT_OFFSET_DEFAULT_MS, MIDI_OUT_OFFSET_MAX_MS};
use super::paths::{SETTINGS_FILE_NAME, config_dir};

/// The on-disk `settings.json` shape.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SettingsData {
    /// Last-used MIDI input port name, reconnected at startup.
    pub(crate) midi_in_port: Option<String>,
    /// Last-used MIDI output port name, reconnected at startup.
    pub(crate) midi_out_port: Option<String>,
    /// The current project folder (where ⌘/Ctrl+N/S put a new project),
    /// set from the browser panel.
    #[serde(default)]
    pub(crate) last_project_folder: Option<String>,
    /// Delay applied to clip MIDI leaving the output port, in milliseconds —
    /// see `config::MIDI_OUT_OFFSET_DEFAULT_MS` and `160-midi-out-offset.md`.
    /// Absent in projects saved before the setting existed, which is why the
    /// default comes from `default_midi_out_offset_ms` rather than `i32`'s
    /// zero: an upgrading user should get the aligned behaviour, not the old
    /// undelayed one.
    #[serde(default = "default_midi_out_offset_ms")]
    pub(crate) midi_out_offset_ms: i32,
    /// Index into `view::theme`'s palette list, applied at startup. Absent in
    /// settings saved before themes existed, hence `#[serde(default)]`
    /// rather than requiring the field.
    #[serde(default)]
    pub(crate) theme_index: usize,
}

/// Serde default for [`SettingsData::midi_out_offset_ms`] — the aligned offset,
/// not `0`.
fn default_midi_out_offset_ms() -> i32 {
    MIDI_OUT_OFFSET_DEFAULT_MS
}

/// Hand-written rather than derived: a fresh install (no settings file, or an
/// unparseable one) must land on the same aligned offset as an upgrading user,
/// not on `i32::default()`.
impl Default for SettingsData {
    fn default() -> Self {
        SettingsData {
            midi_in_port: None,
            midi_out_port: None,
            last_project_folder: None,
            midi_out_offset_ms: default_midi_out_offset_ms(),
            theme_index: 0,
        }
    }
}

/// Clamps a user-supplied offset into the supported range.
pub(crate) fn clamp_midi_out_offset_ms(value: i32) -> i32 {
    value.clamp(0, MIDI_OUT_OFFSET_MAX_MS)
}

/// Path to `settings.json` in the app's config directory.
fn settings_path() -> PathBuf {
    config_dir().join(SETTINGS_FILE_NAME)
}

/// Reads `settings.json`, falling back to [`SettingsData::default`] on a
/// missing or unparseable file.
pub(crate) fn load_settings() -> SettingsData {
    let path = settings_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return SettingsData::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Writes `settings.json`.
fn save_settings(data: &SettingsData) -> io::Result<()> {
    let path = settings_path();
    let text = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
    fs::write(path, text)
}

/// Reads `settings.json`, applies `change` and writes it back, logging a
/// failed write as a failure to save `what`.
pub(crate) fn update_settings(what: &str, change: impl FnOnce(&mut SettingsData)) {
    let mut settings = load_settings();
    change(&mut settings);
    if let Err(e) = save_settings(&settings) {
        eprintln!("Failed to save {what}: {e}");
    }
}
