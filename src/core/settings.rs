//! App-wide settings (not project data): the audio output, rate and buffer,
//! the remembered MIDI ports, the MIDI-out offset, the last project folder,
//! the recent projects, the theme index, the browser panel's width and the
//! keyboard shortcuts the user rebound. Serialized to
//! `settings.json` in the platform config directory (`paths::config_dir`).
//! Like the project DTOs, a new field must be `#[serde(default)]` — but here
//! the default is often *not* the zero value, so an upgrading user gets
//! current behaviour.

use std::{collections::BTreeMap, fs, io, path::PathBuf};

use log::error;
use serde::{Deserialize, Serialize};

use super::audio::{AudioDevice, AudioPrefs, DESIRED_BUFFER_FRAMES};
use super::config::{BROWSER_W_DEFAULT, MIDI_OUT_OFFSET_DEFAULT_MS, MIDI_OUT_OFFSET_MAX_MS};
use super::keymap::ChordDto;
use super::paths::{SETTINGS_FILE_NAME, config_dir};
use super::project::ProjectOrigin;

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
    /// The browser panel's width in points, as its edge was last dragged
    /// (clamped where it's read). Absent in settings saved before the edge
    /// could be dragged: the old fixed width.
    #[serde(default = "default_browser_width")]
    pub(crate) browser_width: f32,
    /// The keyboard shortcuts the user changed, by action id
    /// (`core/keymap.rs`): only the actions whose chords differ from their
    /// defaults, an empty list meaning unbound, so a new default reaches
    /// every action the user never touched. Absent before rebinding
    /// existed: the defaults.
    #[serde(default)]
    pub(crate) keymap: BTreeMap<String, Vec<ChordDto>>,
    /// The projects opened or saved most recently, newest first, at most
    /// `config::RECENT_PROJECTS_MAX`: the browser's Recent category
    /// (`core/project/recent.rs`). Absent before the list existed: empty.
    #[serde(default)]
    pub(crate) recent_projects: Vec<ProjectOrigin>,
    /// *Allow AI agents*: whether the agent bridge's socket is open
    /// (`360-mcp-server.md` decision 4). Off until the user turns it on, and
    /// absent before the bridge existed: off.
    #[serde(default)]
    pub(crate) agent_bridge: bool,
    /// The Audio tab's output device (`480-audio-settings.md` decision 4):
    /// its persistent ID and its name, for the list while it isn't
    /// connected. `None`, and absent before the tab existed: System default.
    #[serde(default)]
    pub(crate) audio_output_device: Option<AudioDevice>,
    /// The Audio tab's sample rate, Hz. `None`, and absent before the tab
    /// existed: whatever the device runs at, never changed.
    #[serde(default)]
    pub(crate) audio_sample_rate: Option<u32>,
    /// The Audio tab's buffer size, frames. Absent before the tab existed:
    /// the 256 the engine always asked for.
    #[serde(default = "default_audio_buffer_frames")]
    pub(crate) audio_buffer_frames: u32,
}

impl SettingsData {
    /// The Audio tab's choices, as the engine takes them.
    pub(crate) fn audio_prefs(&self) -> AudioPrefs {
        AudioPrefs {
            device: self.audio_output_device.clone(),
            sample_rate: self.audio_sample_rate,
            buffer_frames: self.audio_buffer_frames,
        }
    }

    /// Keeps `prefs` as the Audio tab's choices.
    pub(crate) fn set_audio_prefs(&mut self, prefs: &AudioPrefs) {
        self.audio_output_device = prefs.device.clone();
        self.audio_sample_rate = prefs.sample_rate;
        self.audio_buffer_frames = prefs.buffer_frames;
    }
}

/// Serde default for [`SettingsData::audio_buffer_frames`]: the engine's
/// buffer before it could be chosen.
fn default_audio_buffer_frames() -> u32 {
    DESIRED_BUFFER_FRAMES
}

/// Serde default for [`SettingsData::browser_width`].
fn default_browser_width() -> f32 {
    BROWSER_W_DEFAULT
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
            browser_width: default_browser_width(),
            keymap: BTreeMap::new(),
            recent_projects: Vec::new(),
            agent_bridge: false,
            audio_output_device: None,
            audio_sample_rate: None,
            audio_buffer_frames: default_audio_buffer_frames(),
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
        error!("Failed to save {what}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings saved before the Recent list existed load with it empty;
    /// one saved with it reads it back.
    #[test]
    fn the_recent_list_defaults_to_empty_and_round_trips() {
        let old: SettingsData =
            serde_json::from_str(r#"{"midi_in_port":null,"midi_out_port":null}"#).unwrap();
        assert!(old.recent_projects.is_empty());

        let settings = SettingsData {
            recent_projects: vec![
                ProjectOrigin::new(Some("Jams"), "riff"),
                ProjectOrigin::new(None, "loose"),
            ],
            ..SettingsData::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let back: SettingsData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.recent_projects, settings.recent_projects);
    }

    /// Settings saved before the Audio tab existed run as the app did: the
    /// system default at its own rate, 256 frames. A choice reads back.
    #[test]
    fn the_audio_choice_defaults_to_the_old_engine_and_round_trips() {
        let old: SettingsData =
            serde_json::from_str(r#"{"midi_in_port":null,"midi_out_port":null}"#).unwrap();
        assert_eq!(old.audio_prefs(), AudioPrefs::default());
        assert_eq!(old.audio_buffer_frames, 256);

        let prefs = AudioPrefs {
            device: Some(AudioDevice {
                id: "coreaudio:AppleUSBAudioEngine:Focusrite:Scarlett".into(),
                name: "Scarlett 2i2".into(),
            }),
            sample_rate: Some(96_000),
            buffer_frames: 128,
        };
        let mut settings = SettingsData::default();
        settings.set_audio_prefs(&prefs);
        let json = serde_json::to_string(&settings).unwrap();
        let back: SettingsData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.audio_prefs(), prefs);
    }

    /// The agent bridge is off in settings saved before it existed, and
    /// on once turned on.
    #[test]
    fn the_agent_bridge_defaults_to_off_and_round_trips() {
        let old: SettingsData =
            serde_json::from_str(r#"{"midi_in_port":null,"midi_out_port":null}"#).unwrap();
        assert!(!old.agent_bridge);
        assert!(!SettingsData::default().agent_bridge);

        let settings = SettingsData {
            agent_bridge: true,
            ..SettingsData::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            serde_json::from_str::<SettingsData>(&json)
                .unwrap()
                .agent_bridge
        );
    }
}
