//! What the user chose in Settings' Audio tab, and what the engine tells the
//! tab back: the output devices there are and the stream it is running
//! (`480-audio-settings.md` decisions 1–4). The choice is saved in
//! `settings.json`, never in a project. Plus the pure helpers the engine and
//! the tab share: which rates and buffer sizes to offer, and their labels.

use serde::{Deserialize, Serialize};

use super::{DESIRED_BUFFER_FRAMES, MAX_FRAMES};

/// The buffer sizes the tab offers, before the device's range limits them.
const BUFFER_CHOICES: [u32; 7] = [32, 64, 128, 256, 512, 1024, 2048];

/// The rates offered on a device that reports a continuous range rather
/// than a list (an aggregate or virtual device).
const STANDARD_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// The lowest rate offered when the device has any at or above it: the
/// telephone and speech rates some devices list aren't for music.
const LOWEST_MUSIC_RATE: u32 = 44_100;

/// An output device, as the tab lists it and the settings keep it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AudioDevice {
    /// cpal's persistent device ID as a string (`DeviceId`'s `Display`:
    /// the host, then on macOS the CoreAudio UID), which survives a replug
    /// and a reboot.
    pub(crate) id: String,
    /// The name macOS shows, for the list and for a device that isn't
    /// connected.
    pub(crate) name: String,
}

/// What the user chose: the device, the rate and the buffer. Saved in
/// `settings.json` (`SettingsData::audio_prefs`); the default is how the
/// app ran before the tab existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioPrefs {
    /// The chosen output, `None` for System default (followed as it moves).
    pub(crate) device: Option<AudioDevice>,
    /// The chosen rate, Hz; `None` runs at whatever the device runs at and
    /// never changes it (decision 3).
    pub(crate) sample_rate: Option<u32>,
    /// The buffer asked of the device, frames.
    pub(crate) buffer_frames: u32,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            device: None,
            sample_rate: None,
            buffer_frames: DESIRED_BUFFER_FRAMES,
        }
    }
}

/// The stream a restart opened, as the tab shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunningAudio {
    /// The device it runs on. Its ID is empty when the device wouldn't give
    /// one.
    pub(crate) device: AudioDevice,
    /// Whether that is the device the user chose, rather than the system
    /// default followed (System default chosen, or the chosen one missing).
    pub(crate) on_chosen: bool,
    /// The rate it opened at, Hz.
    pub(crate) sample_rate: u32,
    /// The rates the device offers, ascending ([`rate_choices`]).
    pub(crate) rates: Vec<u32>,
    /// The buffer asked of the device, frames; `None` when the device picks
    /// its own (it reports no range, or refused the one asked).
    pub(crate) buffer: Option<u32>,
    /// The buffer sizes the device takes, ascending ([`buffer_choices`]).
    pub(crate) buffers: Vec<u32>,
}

/// What the engine tells the UI.
#[derive(Debug, Clone)]
pub(crate) enum EngineNews {
    /// A restart finished: the stream it opened, if any, and the footer line
    /// for an outside change (decision 11).
    Restarted {
        /// The running stream, `None` with no device open.
        running: Option<RunningAudio>,
        /// The footer line, if the change came from outside.
        message: Option<String>,
    },
    /// The output devices changed (or the first list, at launch).
    Devices {
        /// Every output device, in the host's order.
        devices: Vec<AudioDevice>,
        /// The system default's name, for the System default row.
        default_name: Option<String>,
    },
}

/// The rates to offer for a device reporting `ranges` (min, max Hz): each
/// discrete rate, and the standard rates inside a continuous range, from
/// 44.1 kHz up when there are any. Ascending, no repeats. Pure, unit-tested.
pub(crate) fn rate_choices(ranges: &[(u32, u32)]) -> Vec<u32> {
    let mut rates: Vec<u32> = ranges
        .iter()
        .flat_map(|&(min, max)| {
            let standard = STANDARD_RATES
                .into_iter()
                .filter(move |r| min < max && (min..=max).contains(r));
            (min == max).then_some(min).into_iter().chain(standard)
        })
        .collect();
    rates.sort_unstable();
    rates.dedup();
    if rates.iter().any(|&r| r >= LOWEST_MUSIC_RATE) {
        rates.retain(|&r| r >= LOWEST_MUSIC_RATE);
    }
    rates
}

/// The buffer sizes to offer for a device taking `min..=max` frames: the
/// standard sizes inside it and within [`MAX_FRAMES`]. With no range, none:
/// the device picks its own. Pure, unit-tested.
pub(crate) fn buffer_choices(range: Option<(u32, u32)>) -> Vec<u32> {
    let Some((min, max)) = range else {
        return Vec::new();
    };
    BUFFER_CHOICES
        .into_iter()
        .filter(|&frames| (min..=max).contains(&frames) && frames as usize <= MAX_FRAMES)
        .collect()
}

/// `44100` as "44.1 kHz", `48000` as "48 kHz". Pure, unit-tested.
pub(crate) fn khz(hz: u32) -> String {
    format!("{} kHz", khz_number(hz))
}

/// `44100` as "44.1", `48000` as "48": [`khz`] without the unit.
pub(crate) fn khz_number(hz: u32) -> String {
    let khz = f64::from(hz) / 1000.0;
    format!("{}", (khz * 10.0).round() / 10.0)
}

/// A buffer of `frames` at `hz` as "5.3 ms". Pure, unit-tested.
pub(crate) fn buffer_ms(frames: u32, hz: u32) -> String {
    if hz == 0 {
        return "–".to_string();
    }
    format!("{:.1} ms", f64::from(frames) * 1000.0 / f64::from(hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discrete_rates_are_listed_from_44_1_khz_up() {
        let ranges = [
            (8_000, 8_000),
            (44_100, 44_100),
            (48_000, 48_000),
            (96_000, 96_000),
            (48_000, 48_000),
        ];
        assert_eq!(rate_choices(&ranges), vec![44_100, 48_000, 96_000]);
    }

    #[test]
    fn a_continuous_range_offers_the_standard_rates_inside_it() {
        assert_eq!(
            rate_choices(&[(44_100, 96_000)]),
            vec![44_100, 48_000, 88_200, 96_000]
        );
    }

    #[test]
    fn a_device_with_only_low_rates_still_offers_them() {
        assert_eq!(
            rate_choices(&[(16_000, 16_000), (8_000, 8_000)]),
            vec![8_000, 16_000]
        );
    }

    #[test]
    fn buffer_sizes_are_limited_to_the_device_range() {
        assert_eq!(buffer_choices(Some((64, 512))), vec![64, 128, 256, 512]);
        assert_eq!(buffer_choices(Some((15, 4096))), BUFFER_CHOICES.to_vec());
        assert!(buffer_choices(None).is_empty());
    }

    #[test]
    fn rates_read_in_khz() {
        assert_eq!(khz(44_100), "44.1 kHz");
        assert_eq!(khz(48_000), "48 kHz");
        assert_eq!(khz(88_200), "88.2 kHz");
        assert_eq!(khz(96_000), "96 kHz");
        assert_eq!(khz_number(176_400), "176.4");
    }

    #[test]
    fn a_buffer_reads_as_its_length_at_the_rate() {
        assert_eq!(buffer_ms(256, 48_000), "5.3 ms");
        assert_eq!(buffer_ms(512, 44_100), "11.6 ms");
        assert_eq!(buffer_ms(256, 0), "–");
    }
}
