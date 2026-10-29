//! The settings modal's AI tab state: the *Allow AI agents* switch, the
//! `"agent-bridge"` thread's control channel, and the command that adds
//! Stev to Claude Code, which a click copies. The tab also says what the
//! feature is and how to connect a client; drawing and the hit-test are
//! `rendering/modals/settings.rs`. See `360-mcp-server.md` and
//! `030-ui-design.md` § Settings modal.

use std::env;

use crossbeam_channel::Sender;
use egui::Context;

use crate::core::agent::MCP_FLAG;

use super::Display;

/// The binary's path when the running one can't be found: where the app
/// installs.
const INSTALLED_EXE: &str = "/Applications/Stev.app/Contents/MacOS/stev";

/// In the path of an app macOS runs from a temporary, read-only copy (App
/// Translocation): a downloaded app opened where it was unzipped instead of
/// moved first. The copy's path changes on every launch.
const TRANSLOCATED: &str = "/AppTranslocation/";

/// The settings modal's AI tab state, held as `SettingsModal::ai`.
pub(super) struct AiTab {
    /// *Allow AI agents*, as saved (`SettingsData::agent_bridge`).
    pub(super) agent_bridge: bool,
    /// To the `"agent-bridge"` thread: the switch flipped. `None` until
    /// `main` attaches it ([`Display::attach_agent_bridge`]).
    agent_bridge_tx: Option<Sender<bool>>,
    /// The Terminal command that adds Stev to Claude Code for every folder
    /// (`--scope user`; the default scope covers only the folder it is run
    /// in), naming this binary, so a build run from elsewhere shows its own
    /// path. `None` while macOS runs a translocated copy, whose path would
    /// stop working at the next launch.
    pub(super) claude_code_command: Option<String>,
    /// A click asked for the command on the clipboard; the next frame puts
    /// it there ([`Display::sync_ai_copy`]).
    copy_requested: bool,
    /// The command went to the clipboard since the modal opened.
    pub(super) copied: bool,
}

impl Default for AiTab {
    /// The switch off and no channel, until `main` attaches both.
    fn default() -> Self {
        let exe = env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| INSTALLED_EXE.to_owned());
        AiTab {
            agent_bridge: false,
            agent_bridge_tx: None,
            claude_code_command: claude_code_command(&exe),
            copy_requested: false,
            copied: false,
        }
    }
}

impl AiTab {
    /// Takes the `"agent-bridge"` thread's control channel and the saved
    /// switch.
    pub(super) fn attach(&mut self, tx: Sender<bool>, enabled: bool) {
        self.agent_bridge_tx = Some(tx);
        self.agent_bridge = enabled;
    }

    /// Flips the switch and tells the `"agent-bridge"` thread, which opens
    /// or closes its socket at once. Returns the new state, for saving.
    pub(super) fn toggle(&mut self) -> bool {
        self.agent_bridge = !self.agent_bridge;
        if let Some(tx) = &self.agent_bridge_tx {
            tx.send(self.agent_bridge).ok();
        }
        self.agent_bridge
    }

    /// Asks the next frame to put the Claude Code command on the clipboard.
    pub(super) fn request_copy(&mut self) {
        self.copy_requested = true;
    }

    /// The command, once, if a copy was asked for — marked copied.
    fn take_copy(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.copy_requested) {
            return None;
        }
        let command = self.claude_code_command.clone()?;
        self.copied = true;
        Some(command)
    }
}

/// `claude mcp add` for the binary at `exe`, quoted for the shell when its
/// path needs it — `None` when `exe` is a translocated copy.
fn claude_code_command(exe: &str) -> Option<String> {
    (!exe.contains(TRANSLOCATED)).then(|| {
        format!(
            "claude mcp add --scope user stev -- {} {MCP_FLAG}",
            shell_quote(exe)
        )
    })
}

/// `text` as one shell word: as it is when every character is safe, else
/// in single quotes, each `'` in it closed, escaped and reopened.
fn shell_quote(text: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%".contains(c);
    if !text.is_empty() && text.chars().all(safe) {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

impl Display {
    /// Once a frame: puts the Claude Code command on the clipboard when a
    /// click on it asked.
    pub(super) fn sync_ai_copy(&mut self, ctx: &Context) {
        if let Some(command) = self.settings.ai.take_copy() {
            ctx.copy_text(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;

    use super::{AiTab, claude_code_command, shell_quote};

    #[test]
    fn a_flip_tells_the_bridge_each_new_state() {
        let (tx, rx) = unbounded();
        let mut tab = AiTab::default();
        tab.attach(tx, true);
        assert!(!tab.toggle());
        assert!(tab.toggle());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn a_flip_before_the_bridge_is_attached_still_flips() {
        let mut tab = AiTab::default();
        assert!(tab.toggle());
        assert!(tab.agent_bridge);
    }

    #[test]
    fn the_command_adds_the_server_for_every_folder() {
        assert_eq!(
            claude_code_command("/Applications/Stev.app/Contents/MacOS/stev").as_deref(),
            Some(
                "claude mcp add --scope user stev -- /Applications/Stev.app/Contents/MacOS/stev --mcp"
            )
        );
    }

    #[test]
    fn a_translocated_copy_gets_no_command() {
        let exe = "/private/var/folders/xy/abc/T/AppTranslocation/0A1B2C3D/d/Stev.app/Contents/MacOS/stev";
        assert_eq!(claude_code_command(exe), None);
    }

    #[test]
    fn a_path_with_spaces_or_quotes_stays_one_word() {
        assert_eq!(
            shell_quote("/Applications/Stev 2.app/stev"),
            "'/Applications/Stev 2.app/stev'"
        );
        assert_eq!(shell_quote("/tmp/it's/stev"), r"'/tmp/it'\''s/stev'");
    }

    #[test]
    fn a_copy_is_taken_once_and_marks_the_command_copied() {
        let mut tab = AiTab::default();
        assert_eq!(tab.take_copy(), None);
        tab.request_copy();
        assert_eq!(tab.take_copy(), tab.claude_code_command.clone());
        assert!(tab.copied);
        assert_eq!(tab.take_copy(), None);
    }

    #[test]
    fn without_a_command_a_copy_does_nothing() {
        let mut tab = AiTab {
            claude_code_command: None,
            ..AiTab::default()
        };
        tab.request_copy();
        assert_eq!(tab.take_copy(), None);
        assert!(!tab.copied);
    }
}
