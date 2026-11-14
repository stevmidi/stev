//! The settings modal's Audio tab state (`480-audio-settings.md` decision 1):
//! the output-device list (System default first, then every device), the
//! sample rates and buffer sizes the running device offers, the user's
//! choice, which section has the focus with a cursor in each, and what the
//! engine reports back — the stream it runs and, live, its measured buffer
//! and latency.
//!
//! Held by the settings modal as `SettingsModal::audio`; the methods that
//! act on a choice stay on [`Display`](crate::view::display::Display) and
//! reach in through `self.settings.audio`. A choice is applied at once
//! (decision 2): the engine restarts on it, and it is saved.

use std::sync::Arc;

use crate::core::audio::{
    AudioControl, AudioDevice, AudioPrefs, RunningAudio, StreamStatus, buffer_ms, khz,
};

use super::window_start;
use crate::view::display::AudioSettingsFocus;
use crate::view::display::browser::scroll_to_show;

/// Device rows the list shows at once, System default included; a longer
/// list scrolls.
pub(in crate::view::display) const DEVICE_ROWS: usize = 4;

/// A pick in the Audio tab, by index into what the tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::view::display) enum AudioChoice {
    /// A device row: `0` is System default, then the devices in order.
    Device(usize),
    /// A sample rate, by index into the running device's rates.
    Rate(usize),
    /// A buffer size, by index into the running device's sizes.
    Buffer(usize),
}

/// The settings modal's Audio tab state, held as `SettingsModal::audio`.
pub(in crate::view::display) struct AudioTab {
    /// Every output device, refreshed live from
    /// `UiEvent::AudioDevicesRefreshed`.
    pub(in crate::view::display) devices: Vec<AudioDevice>,
    /// The system default's name, for the System default row.
    pub(in crate::view::display) default_name: Option<String>,
    /// The stream the engine last opened (`UiEvent::AudioRunning`); `None`
    /// before the first report, or with no device open.
    pub(in crate::view::display) running: Option<RunningAudio>,
    /// The user's choice, as saved.
    pub(in crate::view::display) prefs: AudioPrefs,
    /// Which section the keys act on.
    pub(in crate::view::display) focus: AudioSettingsFocus,
    /// The device list's cursor row (`0` is System default).
    pub(in crate::view::display) device_cursor: usize,
    /// First row of the [`DEVICE_ROWS`] window.
    device_scroll: usize,
    /// The cursor among the rates.
    pub(in crate::view::display) rate_cursor: usize,
    /// The cursor among the buffer sizes.
    pub(in crate::view::display) buffer_cursor: usize,
    /// The engine's line, for a choice; `None` until `main` attaches it.
    control: Option<AudioControl>,
    /// The engine's live measurements, for the readout.
    pub(in crate::view::display) status: Option<Arc<StreamStatus>>,
}

impl Default for AudioTab {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            default_name: None,
            running: None,
            prefs: AudioPrefs::default(),
            focus: AudioSettingsFocus::Device,
            device_cursor: 0,
            device_scroll: 0,
            rate_cursor: 0,
            buffer_cursor: 0,
            control: None,
            status: None,
        }
    }
}

impl AudioTab {
    /// Takes the engine's line and measurements, and the choice it was
    /// started with.
    pub(in crate::view::display) fn attach(
        &mut self,
        control: AudioControl,
        status: Arc<StreamStatus>,
        prefs: AudioPrefs,
    ) {
        self.control = Some(control);
        self.status = Some(status);
        self.prefs = prefs;
    }

    /// Replaces the device list (hot-plug), keeping the cursor inside it.
    pub(in crate::view::display) fn set_devices(
        &mut self,
        devices: Vec<AudioDevice>,
        default_name: Option<String>,
    ) {
        self.devices = devices;
        self.default_name = default_name;
        self.select_device(self.device_cursor);
    }

    /// Takes the engine's report of a restart: the cursors go to what runs
    /// now.
    pub(in crate::view::display) fn set_running(&mut self, running: Option<RunningAudio>) {
        self.running = running;
        self.select_current();
    }

    /// Puts every cursor on what runs: the device row in use, the running
    /// rate and buffer.
    pub(in crate::view::display) fn select_current(&mut self) {
        self.select_device(self.active_row().unwrap_or(0));
        self.rate_cursor = self.active_rate().unwrap_or(0);
        self.buffer_cursor = self.active_buffer().unwrap_or(0);
    }

    /// How many device rows there are: System default and every device.
    pub(in crate::view::display) fn row_count(&self) -> usize {
        1 + self.devices.len()
    }

    /// Device row `idx`'s text: "System default — MacBook Pro Speakers",
    /// then the device names.
    pub(in crate::view::display) fn row_label(&self, idx: usize) -> String {
        match idx.checked_sub(1) {
            None => match &self.default_name {
                Some(name) => format!("System default — {name}"),
                None => "System default".to_string(),
            },
            Some(i) => self
                .devices
                .get(i)
                .map(|device| device.name.clone())
                .unwrap_or_default(),
        }
    }

    /// The device row the stream runs on: the chosen device's when it runs
    /// on it, else System default's; `None` with no stream.
    pub(in crate::view::display) fn active_row(&self) -> Option<usize> {
        let running = self.running.as_ref()?;
        if !running.on_chosen {
            return Some(0);
        }
        self.devices
            .iter()
            .position(|device| device.id == running.device.id)
            .map(|i| i + 1)
    }

    /// The chosen device's name while it isn't connected — shown as a ghost
    /// row under the list, switched to when it comes back (decision 4).
    pub(in crate::view::display) fn missing(&self) -> Option<&str> {
        let chosen = self.prefs.device.as_ref()?;
        let connected = self.devices.iter().any(|device| device.id == chosen.id);
        (!connected).then_some(chosen.name.as_str())
    }

    /// Puts the device cursor on row `idx`, kept inside the list and in view.
    pub(in crate::view::display) fn select_device(&mut self, idx: usize) {
        self.device_cursor = idx.min(self.row_count() - 1);
        self.device_scroll = scroll_to_show(self.device_scroll, self.device_cursor, DEVICE_ROWS);
    }

    /// Moves the device cursor by `step` rows, stopping at either end.
    pub(in crate::view::display) fn move_device(&mut self, step: isize) {
        self.select_device(self.device_cursor.saturating_add_signed(step));
    }

    /// The first device row the window shows.
    pub(in crate::view::display) fn window_start(&self) -> usize {
        window_start(self.device_scroll, self.row_count(), DEVICE_ROWS)
    }

    /// How many device rows the window shows.
    pub(in crate::view::display) fn visible(&self) -> usize {
        self.row_count().min(DEVICE_ROWS)
    }

    /// The running device's rates, Hz; none with no stream.
    pub(in crate::view::display) fn rates(&self) -> &[u32] {
        self.running.as_ref().map_or(&[], |r| r.rates.as_slice())
    }

    /// The running device's buffer sizes, frames; none with no stream or a
    /// device that picks its own.
    pub(in crate::view::display) fn buffers(&self) -> &[u32] {
        self.running.as_ref().map_or(&[], |r| r.buffers.as_slice())
    }

    /// The running rate's index among [`rates`](Self::rates).
    pub(in crate::view::display) fn active_rate(&self) -> Option<usize> {
        let hz = self.running.as_ref()?.sample_rate;
        self.rates().iter().position(|&r| r == hz)
    }

    /// The running buffer's index among [`buffers`](Self::buffers): the
    /// size asked of the device.
    pub(in crate::view::display) fn active_buffer(&self) -> Option<usize> {
        let frames = self.running.as_ref()?.buffer?;
        self.buffers().iter().position(|&b| b == frames)
    }

    /// Moves the cursor of the focused row of choices by `step`, stopping at
    /// either end.
    pub(in crate::view::display) fn move_choice(&mut self, step: isize) {
        let (rates, buffers) = (self.rates().len(), self.buffers().len());
        let (cursor, len) = match self.focus {
            AudioSettingsFocus::Device => return,
            AudioSettingsFocus::Rate => (&mut self.rate_cursor, rates),
            AudioSettingsFocus::Buffer => (&mut self.buffer_cursor, buffers),
        };
        *cursor = cursor
            .saturating_add_signed(step)
            .min(len.saturating_sub(1));
    }

    /// The choice under the focused section's cursor.
    pub(in crate::view::display) fn cursor_choice(&self) -> AudioChoice {
        match self.focus {
            AudioSettingsFocus::Device => AudioChoice::Device(self.device_cursor),
            AudioSettingsFocus::Rate => AudioChoice::Rate(self.rate_cursor),
            AudioSettingsFocus::Buffer => AudioChoice::Buffer(self.buffer_cursor),
        }
    }

    /// Makes `choice` the user's: the focus and the cursor go to it, and the
    /// new choice comes back to apply and save — `None` when it is what was
    /// already chosen, or nothing (an index past the list).
    pub(in crate::view::display) fn choose(&mut self, choice: AudioChoice) -> Option<AudioPrefs> {
        let mut prefs = self.prefs.clone();
        match choice {
            AudioChoice::Device(idx) => {
                self.focus = AudioSettingsFocus::Device;
                self.select_device(idx);
                prefs.device = match idx.checked_sub(1) {
                    None => None,
                    Some(i) => Some(self.devices.get(i)?.clone()),
                };
            }
            AudioChoice::Rate(i) => {
                self.focus = AudioSettingsFocus::Rate;
                prefs.sample_rate = Some(*self.rates().get(i)?);
                self.rate_cursor = i;
            }
            AudioChoice::Buffer(i) => {
                self.focus = AudioSettingsFocus::Buffer;
                prefs.buffer_frames = *self.buffers().get(i)?;
                self.buffer_cursor = i;
            }
        }
        if prefs == self.prefs {
            return None;
        }
        self.prefs = prefs.clone();
        Some(prefs)
    }

    /// Has the engine run as `prefs` say.
    pub(in crate::view::display) fn apply(&self, prefs: &AudioPrefs) {
        if let Some(control) = &self.control {
            control.configure(prefs.clone());
        }
    }

    /// The readout of what runs, and a dim note on what differs from the
    /// choice.
    pub(in crate::view::display) fn readout(&self) -> Readout {
        let status = self.status.as_deref();
        readout(
            self.running.as_ref(),
            &self.prefs,
            status.and_then(StreamStatus::buffer_frames),
            status.and_then(StreamStatus::latency_ms),
        )
    }

    /// Whether the readout still waits on the stream's first callback for
    /// its buffer and latency, so the tab keeps redrawing until they come.
    pub(in crate::view::display) fn awaiting_measurements(&self) -> bool {
        self.running.is_some()
            && self
                .status
                .as_deref()
                .is_some_and(|status| status.buffer_frames().is_none())
    }
}

/// The Audio tab's readout of what runs, a line each.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::view::display) struct Readout {
    /// The device, rate and buffer: "MacBook Pro Speakers · 48 kHz · 256
    /// frames", or "No audio output".
    pub(in crate::view::display) running: String,
    /// "Output latency 11.2 ms", once the first callback has said.
    pub(in crate::view::display) latency: Option<String>,
    /// What isn't as chosen: dim.
    pub(in crate::view::display) note: Option<String>,
}

/// The readout for `running` chosen as `prefs`, with the buffer the device
/// runs at (`measured`, once the first callback has said) and the output
/// latency in ms. Pure, unit-tested.
fn readout(
    running: Option<&RunningAudio>,
    prefs: &AudioPrefs,
    measured: Option<u32>,
    latency_ms: Option<f64>,
) -> Readout {
    let Some(running) = running else {
        return Readout {
            running: "No audio output".to_string(),
            latency: None,
            note: None,
        };
    };
    let mut line = format!("{} · {}", running.device.name, khz(running.sample_rate));
    let frames = measured.or(running.buffer);
    if let Some(frames) = frames {
        line += &format!(" · {frames} frames");
    }

    let mut notes = Vec::new();
    if let Some(hz) = prefs.sample_rate
        && hz != running.sample_rate
    {
        notes.push(format!(
            "asked for {}; the device runs at {}",
            khz(hz),
            khz(running.sample_rate)
        ));
    }
    if let Some(frames) = frames
        && frames != prefs.buffer_frames
    {
        notes.push(format!(
            "asked for {}; the device runs at {frames}",
            prefs.buffer_frames
        ));
    }
    Readout {
        running: line,
        latency: latency_ms.map(|ms| format!("Output latency {ms:.1} ms")),
        note: (!notes.is_empty()).then(|| notes.join(" · ")),
    }
}

/// A buffer size's two lines for its choice: the frames, and their length at
/// `hz`.
pub(in crate::view::display) fn buffer_label(frames: u32, hz: u32) -> (String, String) {
    (frames.to_string(), buffer_ms(frames, hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str) -> AudioDevice {
        AudioDevice {
            id: format!("coreaudio:{name}"),
            name: name.to_string(),
        }
    }

    fn running(name: &str, on_chosen: bool) -> RunningAudio {
        RunningAudio {
            device: device(name),
            on_chosen,
            sample_rate: 48_000,
            rates: vec![44_100, 48_000, 96_000],
            buffer: Some(256),
            buffers: vec![64, 128, 256, 512],
        }
    }

    fn tab(names: &[&str]) -> AudioTab {
        let mut tab = AudioTab::default();
        tab.set_devices(
            names.iter().map(|n| device(n)).collect(),
            names.first().map(|n| n.to_string()),
        );
        tab
    }

    #[test]
    fn system_default_heads_the_list_naming_the_default() {
        let tab = tab(&["Speakers", "Scarlett"]);
        assert_eq!(tab.row_count(), 3);
        assert_eq!(tab.row_label(0), "System default — Speakers");
        assert_eq!(tab.row_label(2), "Scarlett");
    }

    #[test]
    fn the_row_in_use_is_the_chosen_device_or_system_default() {
        let mut tab = tab(&["Speakers", "Scarlett"]);
        assert_eq!(tab.active_row(), None);
        tab.set_running(Some(running("Speakers", false)));
        assert_eq!(tab.active_row(), Some(0));
        tab.set_running(Some(running("Scarlett", true)));
        assert_eq!(tab.active_row(), Some(2));
        assert_eq!(tab.device_cursor, 2);
    }

    #[test]
    fn a_chosen_device_not_connected_is_missing() {
        let mut tab = tab(&["Speakers"]);
        assert_eq!(tab.missing(), None);
        tab.prefs.device = Some(device("Scarlett 2i2"));
        assert_eq!(tab.missing(), Some("Scarlett 2i2"));
        tab.set_devices(vec![device("Speakers"), device("Scarlett 2i2")], None);
        assert_eq!(tab.missing(), None);
    }

    #[test]
    fn choosing_sets_the_prefs_once() {
        let mut tab = tab(&["Speakers", "Scarlett"]);
        tab.set_running(Some(running("Speakers", false)));
        let chosen = tab.choose(AudioChoice::Device(2)).unwrap();
        assert_eq!(chosen.device, Some(device("Scarlett")));
        assert_eq!(tab.choose(AudioChoice::Device(2)), None);
        let chosen = tab.choose(AudioChoice::Device(0)).unwrap();
        assert_eq!(chosen.device, None);

        let chosen = tab.choose(AudioChoice::Rate(2)).unwrap();
        assert_eq!(chosen.sample_rate, Some(96_000));
        assert_eq!(tab.focus, AudioSettingsFocus::Rate);
        let chosen = tab.choose(AudioChoice::Buffer(0)).unwrap();
        assert_eq!(chosen.buffer_frames, 64);
        // Past the list: nothing.
        assert_eq!(tab.choose(AudioChoice::Rate(9)), None);
        assert_eq!(tab.choose(AudioChoice::Device(9)), None);
    }

    #[test]
    fn the_choice_cursors_stop_at_both_ends() {
        let mut tab = tab(&["Speakers"]);
        tab.set_running(Some(running("Speakers", false)));
        assert_eq!(tab.rate_cursor, 1);
        assert_eq!(tab.buffer_cursor, 2);
        tab.focus = AudioSettingsFocus::Rate;
        tab.move_choice(5);
        assert_eq!(tab.rate_cursor, 2);
        tab.move_choice(-5);
        assert_eq!(tab.rate_cursor, 0);
        tab.focus = AudioSettingsFocus::Buffer;
        tab.move_choice(1);
        assert_eq!(tab.buffer_cursor, 3);
    }

    #[test]
    fn the_readout_says_what_runs() {
        let prefs = AudioPrefs::default();
        let speakers = running("MacBook Pro Speakers", false);
        assert_eq!(
            readout(Some(&speakers), &prefs, Some(256), Some(11.2)),
            Readout {
                running: "MacBook Pro Speakers · 48 kHz · 256 frames".to_string(),
                latency: Some("Output latency 11.2 ms".to_string()),
                note: None,
            }
        );
        let none = readout(None, &prefs, None, None);
        assert_eq!(none.running, "No audio output");
        assert_eq!(none.latency, None);
    }

    #[test]
    fn the_readout_notes_what_the_device_would_not_give() {
        let prefs = AudioPrefs {
            sample_rate: Some(96_000),
            buffer_frames: 64,
            ..AudioPrefs::default()
        };
        let speakers = running("Speakers", false);
        let note = readout(Some(&speakers), &prefs, Some(512), None).note;
        assert_eq!(
            note.as_deref(),
            Some(
                "asked for 96 kHz; the device runs at 48 kHz · \
                 asked for 64; the device runs at 512"
            )
        );
    }

    #[test]
    fn a_buffer_choice_reads_frames_over_ms() {
        assert_eq!(
            buffer_label(256, 48_000),
            ("256".to_string(), "5.3 ms".to_string())
        );
    }
}
