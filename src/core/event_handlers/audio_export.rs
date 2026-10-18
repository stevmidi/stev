//! `EventHandlers` workflows for the audio export (`⌘/Ctrl+⇧+R`,
//! `300-export-to-audio.md`): the run that plays the arranger's time
//! selection through the plugin host while the `"audio-export"` thread
//! writes a WAV per stem plus the mix.
//!
//! The export is not an edit, so there's no undo. It is a transport run,
//! and it puts the transport back: looping goes off for the run and comes
//! back after, and the cursor is never moved (the run plays from the
//! selection's start with `PlayFromTick`, and the stop at the end returns
//! playback to the cursor).

use std::fs;
use std::path::Path;

use crate::core::{
    input_event::TimeSelectionRect,
    project::{self, MIX_FILE_NAME},
    sequencer::{AudioExportJob, AudioExportOutcome, AudioExportRefusal, AudioExportRun},
};

use super::*;

/// The footer's answer where the plugin host, and so the writer, is missing.
const NEEDS_PLUGIN_HOST: &str = "Audio export needs the plugin host (macOS)";

impl EventHandlers {
    /// `⌘/Ctrl+⇧+R`: plans the export, makes its folder, hands the files to
    /// the `"audio-export"` thread and plays the selection with looping off
    /// (`Sequencer::begin_audio_export`; the tick path sends the marks). The
    /// footer says it started, or why it didn't. The writer's report ends
    /// the run ([`finish_audio_export_workflow`](Self::finish_audio_export_workflow)).
    pub(super) fn export_audio_workflow(
        &self,
        sequencer: &mut Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        name: &str,
    ) {
        let (Ok(message) | Err(message)) = self.start_audio_export(sequencer, time_bounds, name);
        self.send_status_ui_event(message);
    }

    /// The run's start: what the footer says it is doing, or why it refused.
    fn start_audio_export(
        &self,
        sequencer: &mut Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        name: &str,
    ) -> Result<String, String> {
        if cfg!(not(target_os = "macos")) {
            return Err(NEEDS_PLUGIN_HOST.to_owned());
        }
        if sequencer.is_exporting_audio() {
            return Err("An export is already running".to_owned());
        }
        let plan = sequencer
            .audio_export_plan(time_bounds)
            .map_err(|refusal| match refusal {
                AudioExportRefusal::NoSelection if self.view_state() == ViewState::Arranger => {
                    "Select the time to export".to_owned()
                }
                AudioExportRefusal::NoSelection => {
                    "Select the time to export in the arranger".to_owned()
                }
                AudioExportRefusal::NothingToHear => {
                    "Nothing to export: no plugin track plays in the selection".to_owned()
                }
            })?;
        let folder = project::create_audio_export_dir(
            name,
            sequencer.tempo_us(),
            sequencer.meter(),
            plan.start_tick,
        )
        .map_err(|e| format!("Export failed: {e}"))?;
        let job = AudioExportJob {
            stems: plan
                .stems
                .iter()
                .map(|stem| (stem.slot, folder.join(&stem.file_name)))
                .collect(),
            mix: folder.join(MIX_FILE_NAME),
        };
        if self.audio_export_job_tx.send(job).is_err() {
            // Nothing is in it yet.
            fs::remove_dir(&folder).ok();
            return Err(NEEDS_PLUGIN_HOST.to_owned());
        }

        let message = format!("Exporting \"{}\"…", folder_name(&folder));
        let start_tick = plan.start_tick;
        sequencer.begin_audio_export(plan, folder);
        self.send_transport(TransportCommand::SetLoop { enabled: false });
        self.send_transport(TransportCommand::PlayFromTick { tick: start_tick });
        Ok(message)
    }

    /// The `"audio-export"` thread finished the files: stops the transport
    /// (playback back at the cursor), puts the loop state back and says how
    /// it went. A no-op with no run.
    pub(super) fn finish_audio_export_workflow(
        &self,
        sequencer: &mut Sequencer,
        outcome: &AudioExportOutcome,
    ) {
        let Some(run) = sequencer.finish_audio_export() else {
            return;
        };
        self.send_transport(TransportCommand::Stop);
        self.send_transport(TransportCommand::SetLoop {
            enabled: run.loop_was_enabled,
        });
        self.send_status_ui_event(written_message(&run, outcome));
    }
}

/// The export folder's own name, for the footer.
fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The footer's report on a finished export: the folder and what's in it,
/// or the error, plus what the user should know about: stems that came out
/// silent for want of a loaded plugin, MIDI Out tracks that were heard but
/// can't be exported, and dropped samples.
fn written_message(run: &AudioExportRun, outcome: &AudioExportOutcome) -> String {
    let folder = folder_name(&run.folder);
    let head = match &outcome.error {
        Some(error) => format!("Export failed: {error} (what was written is in \"{folder}\")"),
        None => {
            let stems = run.plan.stems.len();
            format!(
                "Exported \"{folder}\" — {stems} stem{} + mix",
                plural(stems)
            )
        }
    };

    let mut notes = Vec::new();
    let silent: Vec<&str> = run
        .plan
        .stems
        .iter()
        .filter(|stem| outcome.silent.contains(&stem.slot))
        .map(|stem| stem.file_name.as_str())
        .collect();
    if !silent.is_empty() {
        notes.push(format!("no plugin loaded: {} silent", silent.join(", ")));
    }
    if !run.plan.left_out.is_empty() {
        let numbers: Vec<String> = run
            .plan
            .left_out
            .iter()
            .map(|position| (position + 1).to_string())
            .collect();
        notes.push(format!(
            "MIDI Out track{} {} left out",
            plural(numbers.len()),
            numbers.join(", ")
        ));
    }
    if outcome.overflowed {
        notes.push("samples were dropped: listen before you use it".to_owned());
    }

    if notes.is_empty() {
        head
    } else {
        format!("{head} ({})", notes.join("; "))
    }
}

/// The plural `s` for `n` of something.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::core::sequencer::test_support::rect;
    #[cfg(target_os = "macos")]
    use crate::core::sequencer::test_support::{clip_at, stub_instrument};
    use crate::core::sequencer::{AudioExportPlan, StemPlan};
    use crate::core::time::Meter;

    use super::super::test_harness::{Harness, harness};
    use super::*;

    const BAR: i32 = Meter::FOUR_FOUR.bars_to_ticks(1);

    fn statuses(h: &Harness) -> Vec<String> {
        h.ui_events
            .try_iter()
            .filter_map(|event| match event {
                UiEvent::Status { message } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn plan(stems: &[(usize, &str)], left_out: Vec<usize>) -> AudioExportPlan {
        AudioExportPlan {
            start_tick: 0,
            end_tick: BAR,
            stems: stems
                .iter()
                .map(|&(slot, name)| StemPlan {
                    slot,
                    file_name: name.to_owned(),
                })
                .collect(),
            left_out,
        }
    }

    fn running(h: &mut Harness, plan: AudioExportPlan) {
        h.sequencer
            .begin_audio_export(plan, PathBuf::from("/x/Exports/Sketch 120bpm 4-4 bar 1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn no_selection_says_where_to_make_one() {
        let mut h = harness();
        h.handlers
            .export_audio_workflow(&mut h.sequencer, None, "Sketch");
        h.handlers.set_view_state(ViewState::Clip);
        h.handlers
            .export_audio_workflow(&mut h.sequencer, None, "Sketch");
        assert_eq!(
            statuses(&h),
            [
                "Select the time to export",
                "Select the time to export in the arranger"
            ]
        );
        assert!(!h.sequencer.is_exporting_audio());
        assert_eq!(h.transport_commands.try_iter().count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nothing_to_hear_refuses_before_touching_the_transport() {
        let mut h = harness();
        h.sequencer.tracks_mut()[0].add_clip(&clip_at(0, BAR));
        let selection = Some(rect(0, BAR, 0, 0));
        h.handlers
            .export_audio_workflow(&mut h.sequencer, selection, "Sketch");
        assert_eq!(
            statuses(&h),
            ["Nothing to export: no plugin track plays in the selection"]
        );
        assert_eq!(h.transport_commands.try_iter().count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_second_export_waits_for_the_first() {
        let mut h = harness();
        h.sequencer.tracks_mut()[0].set_output(stub_instrument(Vec::new()));
        h.sequencer.tracks_mut()[0].add_clip(&clip_at(0, BAR));
        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));
        h.handlers
            .export_audio_workflow(&mut h.sequencer, Some(rect(0, BAR, 0, 0)), "Sketch");
        assert_eq!(statuses(&h), ["An export is already running"]);
        assert_eq!(h.transport_commands.try_iter().count(), 0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_export_needs_the_plugin_host() {
        let mut h = harness();
        h.handlers
            .export_audio_workflow(&mut h.sequencer, Some(rect(0, BAR, 0, 0)), "Sketch");
        assert_eq!(statuses(&h), [NEEDS_PLUGIN_HOST]);
    }

    #[test]
    fn the_writers_report_stops_the_run_and_puts_the_loop_back() {
        let mut h = harness();
        assert!(h.sequencer.is_loop_enabled());
        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));
        h.handlers
            .finish_audio_export_workflow(&mut h.sequencer, &AudioExportOutcome::default());

        assert!(!h.sequencer.is_exporting_audio());
        let commands: Vec<TransportCommand> = h.transport_commands.try_iter().collect();
        assert!(matches!(
            commands[..],
            [
                TransportCommand::Stop,
                TransportCommand::SetLoop { enabled: true }
            ]
        ));
        assert_eq!(
            statuses(&h),
            ["Exported \"Sketch 120bpm 4-4 bar 1\" — 1 stem + mix"]
        );
    }

    #[test]
    fn a_report_with_no_run_does_nothing() {
        let mut h = harness();
        h.handlers
            .finish_audio_export_workflow(&mut h.sequencer, &AudioExportOutcome::default());
        assert_eq!(h.transport_commands.try_iter().count(), 0);
        assert!(statuses(&h).is_empty());
    }

    #[test]
    fn the_report_names_silent_stems_skipped_tracks_and_dropped_samples() {
        let mut h = harness();
        running(
            &mut h,
            plan(&[(0, "01 Bass.wav"), (5, "02 Keys.wav")], vec![2, 3]),
        );
        let run = h.sequencer.finish_audio_export().unwrap();
        let outcome = AudioExportOutcome {
            error: None,
            silent: vec![5],
            overflowed: true,
        };
        assert_eq!(
            written_message(&run, &outcome),
            "Exported \"Sketch 120bpm 4-4 bar 1\" — 2 stems + mix \
             (no plugin loaded: 02 Keys.wav silent; MIDI Out tracks 3, 4 left out; \
             samples were dropped: listen before you use it)"
        );
    }

    #[test]
    fn a_write_error_says_where_the_rest_is() {
        let mut h = harness();
        running(&mut h, plan(&[(0, "01 Bass.wav")], vec![1]));
        let run = h.sequencer.finish_audio_export().unwrap();
        let outcome = AudioExportOutcome {
            error: Some("disk full".to_owned()),
            ..AudioExportOutcome::default()
        };
        assert_eq!(
            written_message(&run, &outcome),
            "Export failed: disk full (what was written is in \"Sketch 120bpm 4-4 bar 1\") \
             (MIDI Out track 2 left out)"
        );
    }
}
