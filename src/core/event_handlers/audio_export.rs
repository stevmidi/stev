//! `EventHandlers` workflows for the audio export (`⌘/Ctrl+⇧+R`,
//! `130-plugin-host.md` § Audio export): the run that plays the arranger's time
//! selection through the plugin host while the `"audio-export"` thread
//! writes a WAV per stem plus the mix.
//!
//! The export is not an edit, so there's no undo. It is a transport run,
//! and it puts the transport back: looping goes off for the run and comes
//! back after, and the cursor is never moved (the run plays from the
//! selection's start with `PlayFromTick`, and the stop at the end returns
//! playback to the cursor).
//!
//! While it runs, [`audio_export_command`](EventHandlers::audio_export_command)
//! and [`audio_export_transport_command`](EventHandlers::audio_export_transport_command)
//! stand in front of every command, as the take session's does
//! (`take_framing.rs`): only looking around and Esc go through. A mute or a
//! fader move halfway through would make the stems disagree with the mix.

use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::core::{
    input_event::TimeSelectionRect,
    project::{self, MIX_FILE_NAME},
    sequencer::{AudioExportJob, AudioExportOutcome, AudioExportRefusal, AudioExportRun},
};

use super::*;

/// The footer's answer where the plugin host, and so the writer, is missing.
const NEEDS_PLUGIN_HOST: &str = "Audio export needs the plugin host (macOS)";

/// The footer's answer to a command the run blocks.
const FINISH_EXPORT_FIRST: &str = "Finish or cancel the export first";

impl EventHandlers {
    /// `⌘/Ctrl+⇧+R`: plans the export, makes its folder, hands the files to
    /// the `"audio-export"` thread and plays the selection with looping off
    /// (`Sequencer::begin_audio_export`; the tick path sends the marks). The
    /// footer shows its progress from there (`AudioExportAtomics`), or says
    /// why it didn't start. The writer's report ends the run
    /// ([`finish_audio_export_workflow`](Self::finish_audio_export_workflow)).
    pub(super) fn export_audio_workflow(
        &self,
        sequencer: &mut Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        name: &str,
    ) {
        if let Err(message) = self.start_audio_export(sequencer, time_bounds, name) {
            self.send_status_ui_event(message);
        }
    }

    /// The run's start, or why it refused.
    fn start_audio_export(
        &self,
        sequencer: &mut Sequencer,
        time_bounds: Option<TimeSelectionRect>,
        name: &str,
    ) -> Result<(), String> {
        if cfg!(not(target_os = "macos")) {
            return Err(NEEDS_PLUGIN_HOST.to_owned());
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
        let stems = plan
            .stems
            .iter()
            .map(|stem| (stem.slot, folder.join(&stem.file_name)))
            .collect();
        let mix = folder.join(MIX_FILE_NAME);
        let start_tick = plan.start_tick;

        let id = sequencer.begin_audio_export(plan, folder.clone());
        let job = AudioExportJob { id, stems, mix };
        if self.audio_export_job_tx.send(job).is_err() {
            // It never played: no marks to close. Nothing is in the folder yet.
            sequencer.finish_audio_export();
            fs::remove_dir(&folder).ok();
            return Err(NEEDS_PLUGIN_HOST.to_owned());
        }
        self.send_transport(TransportCommand::SetLoop { enabled: false });
        self.send_transport(TransportCommand::PlayFromTick { tick: start_tick });
        Ok(())
    }

    /// Esc during a run: the mixer ends the files now and the run is marked
    /// cancelled (`Sequencer::cancel_audio_export`). The writer's report then
    /// ends the run as usual — the transport stopped, the loop state put
    /// back — and removes the files. A no-op with no run.
    pub(super) fn cancel_audio_export_workflow(&self, sequencer: &mut Sequencer) {
        sequencer.cancel_audio_export(Instant::now());
    }

    /// The `"audio-export"` thread is done: stops the transport (playback
    /// back at the cursor), puts the loop state back, removes the files of a
    /// cancelled run or one the writer stopped, and says how it went. A
    /// no-op with no run.
    pub(super) fn finish_audio_export_workflow(
        &self,
        sequencer: &mut Sequencer,
        outcome: &AudioExportOutcome,
    ) {
        // A stopped writer gave up before the mixer ended the export.
        sequencer.close_audio_export(Instant::now());
        let Some(run) = sequencer.finish_audio_export() else {
            return;
        };
        if run.cancelled || outcome.stopped.is_some() {
            remove_export(&run);
        }
        self.send_transport(TransportCommand::Stop);
        self.send_transport(TransportCommand::SetLoop {
            enabled: run.loop_was_enabled,
        });
        self.send_status_ui_event(written_message(&run, outcome));
    }

    /// The run's hold on `cmd`. `true` when it was blocked here and the
    /// ordinary arm must not run; `false` with no run, and for what goes
    /// through: changing view, a plugin's saved state, Esc and the writer's
    /// report.
    pub(super) fn audio_export_command(
        &self,
        cmd: &SequencerCommand,
        sequencer: &Sequencer,
    ) -> bool {
        self.audio_export_blocks(
            sequencer,
            matches!(
                cmd,
                SequencerCommand::AudioExportWritten(_)
                    | SequencerCommand::CancelAudioExport
                    | SequencerCommand::EnterClip
                    | SequencerCommand::ExitClip
                    | SequencerCommand::FocusPane(_)
                    | SequencerCommand::SetTrackInstrumentState { .. }
                    | SequencerCommand::SetMarquee(_)
                    | SequencerCommand::Agent { .. }
            ),
        )
    }

    /// The run's hold on a transport `cmd`, as for
    /// [`audio_export_command`](Self::audio_export_command). Only the run's
    /// own `SetLoop` and `PlayFromTick` go through — nothing else that sends
    /// them can run during an export — and the click, which never reaches
    /// the files. The run's closing `Stop` comes after it has ended.
    pub(super) fn audio_export_transport_command(
        &self,
        cmd: &TransportCommand,
        sequencer: &Sequencer,
    ) -> bool {
        self.audio_export_blocks(
            sequencer,
            matches!(
                cmd,
                TransportCommand::SetLoop { .. }
                    | TransportCommand::PlayFromTick { .. }
                    | TransportCommand::ToggleMetronomeMute
            ),
        )
    }

    /// Whether a running export blocks a command it doesn't `allow`, saying
    /// so in the footer.
    fn audio_export_blocks(&self, sequencer: &Sequencer, allow: bool) -> bool {
        let blocked = sequencer.is_exporting_audio() && !allow;
        if blocked {
            self.send_status_ui_event(FINISH_EXPORT_FIRST.to_owned());
        }
        blocked
    }

    /// Whether an export is running, for the UI thread's key routing (Esc).
    pub(super) fn is_exporting_audio(&self) -> bool {
        self.audio_export.is_running()
    }
}

/// The export folder's own name, for the footer.
fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Removes a cancelled or stopped run's files, then its folder, which held
/// nothing else. A folder that does hold something else stays.
fn remove_export(run: &AudioExportRun) {
    let files = run.plan.stems.iter().map(|stem| stem.file_name.as_str());
    for file in files.chain([MIX_FILE_NAME]) {
        fs::remove_file(run.folder.join(file)).ok();
    }
    fs::remove_dir(&run.folder).ok();
}

/// The footer's report on an export: cancelled, stopped and why, or the
/// folder and what's in it (or the error), plus what the user should know
/// about: stems that came out silent for want of a plugin, MIDI Out tracks
/// that were heard but can't be exported, dropped samples (the one glitch
/// that damages the files) and any overrun or xrun in what was heard.
fn written_message(run: &AudioExportRun, outcome: &AudioExportOutcome) -> String {
    let folder = folder_name(&run.folder);
    if run.cancelled {
        return "Export cancelled".to_owned();
    }
    if let Some(reason) = outcome.stopped {
        return format!("Export stopped: {reason}");
    }
    let head = match &outcome.error {
        Some(error) => {
            format!("Export failed: {error} (what was written is in \"{folder}\")")
        }
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
    // Only dropped samples damage the files. An overrun or xrun glitched
    // what was heard, but the files take the samples before the device.
    if outcome.overflowed {
        notes.push("samples dropped: listen before you use it".to_owned());
    }
    let glitches: Vec<String> = [(outcome.overruns, "overrun"), (outcome.xruns, "xrun")]
        .into_iter()
        .filter(|&(count, _)| count > 0)
        .map(|(count, what)| format!("{count} {what}{}", plural(count as usize)))
        .collect();
    if !glitches.is_empty() {
        notes.push(format!("{} while playing", glitches.join(", ")));
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

    use egui::Key;
    use undo::Record;

    use crate::core::input_event::{InputEvent, KeyModifiers};
    #[cfg(target_os = "macos")]
    use crate::core::sequencer::test_support::clip_at;
    use crate::core::sequencer::test_support::rect;
    use crate::core::sequencer::{AudioExportPlan, StemPlan};
    use crate::core::time::Meter;

    use super::super::test_harness::{Harness, harness, run_command};
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

    /// An export of `plan` running, as `start_audio_export` leaves it.
    fn running(h: &mut Harness, plan: AudioExportPlan) {
        h.sequencer
            .begin_audio_export(plan, PathBuf::from("/x/Exports/Sketch 120bpm 4-4 bar 1"));
    }

    fn finished(h: &mut Harness) -> AudioExportRun {
        h.sequencer.finish_audio_export().unwrap()
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
        assert!(!h.audio_export.is_running());
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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_the_export_needs_the_plugin_host() {
        let mut h = harness();
        h.handlers
            .export_audio_workflow(&mut h.sequencer, Some(rect(0, BAR, 0, 0)), "Sketch");
        assert_eq!(statuses(&h), [NEEDS_PLUGIN_HOST]);
    }

    #[test]
    fn a_running_export_blocks_edits_mixing_and_another_export() {
        let mut h = harness();
        let mut record = Record::new();
        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));
        for cmd in [
            SequencerCommand::ToggleTrackMute { track_idx: 0 },
            SequencerCommand::Commit,
            SequencerCommand::ExportAudio {
                time_bounds: Some(rect(0, BAR, 0, 0)),
                name: "Sketch".to_owned(),
            },
        ] {
            run_command(&mut h, &mut record, cmd);
        }
        assert!(!h.sequencer.track_muted(0));
        assert_eq!(statuses(&h), [FINISH_EXPORT_FIRST; 3]);
        assert_eq!(h.transport_commands.try_iter().count(), 0);

        // Looking around goes through.
        run_command(
            &mut h,
            &mut record,
            SequencerCommand::FocusPane(Pane::Arranger),
        );
        assert!(statuses(&h).is_empty());
    }

    #[test]
    fn a_running_export_keeps_the_transport_to_itself() {
        let mut h = harness();
        let blocked = |h: &Harness, cmd: TransportCommand| {
            h.handlers
                .audio_export_transport_command(&cmd, &h.sequencer)
        };
        assert!(!blocked(&h, TransportCommand::TogglePlayback), "no run");

        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));
        assert!(blocked(&h, TransportCommand::TogglePlayback));
        assert!(blocked(&h, TransportCommand::Stop));
        assert!(blocked(&h, TransportCommand::MoveCursor { ticks: 1 }));
        assert!(!blocked(&h, TransportCommand::SetLoop { enabled: false }));
        assert!(!blocked(&h, TransportCommand::PlayFromTick { tick: 0 }));
        assert!(!blocked(&h, TransportCommand::ToggleMetronomeMute));
        assert_eq!(statuses(&h), [FINISH_EXPORT_FIRST; 3]);
    }

    #[test]
    fn esc_cancels_the_export_and_the_report_puts_everything_back() {
        let mut h = harness();
        let mut record = Record::new();
        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));

        h.handlers.handle_input_event(&InputEvent::KeyPressed {
            key: Key::Escape,
            modifiers: KeyModifiers::default(),
        });
        let cancel = h.sequencer_commands.try_recv().unwrap();
        assert!(matches!(cancel, SequencerCommand::CancelAudioExport));
        run_command(&mut h, &mut record, cancel);
        assert!(h.sequencer.is_exporting_audio(), "until the writer is done");

        run_command(
            &mut h,
            &mut record,
            SequencerCommand::AudioExportWritten(AudioExportOutcome::default()),
        );
        assert!(!h.sequencer.is_exporting_audio());
        assert!(!h.audio_export.is_running());
        let commands: Vec<TransportCommand> = h.transport_commands.try_iter().collect();
        assert!(matches!(
            commands[..],
            [
                TransportCommand::Stop,
                TransportCommand::SetLoop { enabled: true }
            ]
        ));
        assert_eq!(statuses(&h), ["Export cancelled"]);
    }

    #[test]
    fn esc_with_no_export_running_is_the_ordinary_esc() {
        let h = harness();
        h.handlers.set_view_state(ViewState::Clip);
        h.handlers.handle_input_event(&InputEvent::KeyPressed {
            key: Key::Escape,
            modifiers: KeyModifiers::default(),
        });
        assert!(matches!(
            h.sequencer_commands.try_recv(),
            Ok(SequencerCommand::ClearEventSelection)
        ));
    }

    #[test]
    fn the_writers_report_stops_the_run_and_puts_the_loop_back() {
        let mut h = harness();
        assert!(h.sequencer.is_loop_enabled());
        running(&mut h, plan(&[(0, "01 Synth.wav")], Vec::new()));
        h.handlers
            .finish_audio_export_workflow(&mut h.sequencer, &AudioExportOutcome::default());

        assert!(!h.sequencer.is_exporting_audio());
        assert!(!h.audio_export.is_running());
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
    fn the_report_names_silent_stems_skipped_tracks_and_glitches() {
        let mut h = harness();
        running(
            &mut h,
            plan(&[(0, "01 Bass.wav"), (5, "02 Keys.wav")], vec![2, 3]),
        );
        let run = finished(&mut h);
        let outcome = AudioExportOutcome {
            silent: vec![5],
            overflowed: true,
            overruns: 2,
            xruns: 1,
            ..AudioExportOutcome::default()
        };
        assert_eq!(
            written_message(&run, &outcome),
            "Exported \"Sketch 120bpm 4-4 bar 1\" — 2 stems + mix \
             (no plugin loaded: 02 Keys.wav silent; MIDI Out tracks 3, 4 left out; \
             samples dropped: listen before you use it; 2 overruns, 1 xrun while playing)"
        );
    }

    #[test]
    fn a_write_error_says_where_the_rest_is() {
        let mut h = harness();
        running(&mut h, plan(&[(0, "01 Bass.wav")], vec![1]));
        let run = finished(&mut h);
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

    #[test]
    fn a_stopped_export_says_why_and_nothing_else() {
        let mut h = harness();
        running(&mut h, plan(&[(0, "01 Bass.wav")], vec![1]));
        let run = finished(&mut h);
        let outcome = AudioExportOutcome {
            stopped: Some("the audio device changed"),
            overruns: 3,
            ..AudioExportOutcome::default()
        };
        assert_eq!(
            written_message(&run, &outcome),
            "Export stopped: the audio device changed"
        );
    }

    #[test]
    fn a_cancelled_export_removes_its_files_and_folder() {
        let mut h = harness();
        let folder =
            std::env::temp_dir().join(format!("stev-export-cancel-{}", std::process::id()));
        fs::remove_dir_all(&folder).ok();
        fs::create_dir(&folder).unwrap();
        for file in ["01 Synth.wav", MIX_FILE_NAME] {
            fs::write(folder.join(file), b"RIFF").unwrap();
        }
        h.sequencer
            .begin_audio_export(plan(&[(0, "01 Synth.wav")], Vec::new()), folder.clone());
        h.handlers.cancel_audio_export_workflow(&mut h.sequencer);
        h.handlers
            .finish_audio_export_workflow(&mut h.sequencer, &AudioExportOutcome::default());
        assert!(!folder.exists());
        assert_eq!(statuses(&h), ["Export cancelled"]);
    }
}
