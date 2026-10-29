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
    /// What the tab can offer for connecting Claude Code, decided once.
    pub(super) setup: ClaudeCodeSetup,
    /// A click asked for the command on the clipboard; this frame's
    /// [`Display::sync_ai_copy`] puts it there.
    copy_pending: bool,
    /// The command was copied since the modal opened.
    pub(super) copied: bool,
}

/// Whether the AI tab can show the command that adds Stev to Claude Code,
/// and if not, why.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ClaudeCodeSetup {
    /// No MCP server on this platform (it is Unix-only), or the running
    /// binary's path couldn't be read.
    Unsupported,
    /// macOS runs a translocated copy, whose path would stop working at the
    /// next launch: Stev has to move to Applications first.
    Translocated,
    /// The Terminal command that adds Stev to Claude Code for every folder
    /// (`--scope user`; the default scope covers only the folder it is run
    /// in), naming this binary, so a build run from elsewhere shows its own
    /// path.
    Command(String),
}

impl Default for AiTab {
    /// The switch off and no channel, until `main` attaches both.
    fn default() -> Self {
        let setup = match env::current_exe() {
            Ok(exe) if cfg!(unix) => claude_code_setup(&exe.display().to_string()),
            _ => ClaudeCodeSetup::Unsupported,
        };
        AiTab {
            agent_bridge: false,
            agent_bridge_tx: None,
            setup,
            copy_pending: false,
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

    /// The command, when the tab shows one.
    pub(super) fn command(&self) -> Option<&str> {
        match &self.setup {
            ClaudeCodeSetup::Command(command) => Some(command),
            ClaudeCodeSetup::Unsupported | ClaudeCodeSetup::Translocated => None,
        }
    }

    /// A click on the command: marks it copied and has this frame put it on
    /// the clipboard.
    pub(super) fn request_copy(&mut self) {
        self.copy_pending = true;
        self.copied = true;
    }

    /// The command, once, if a copy is pending.
    fn take_copy(&mut self) -> Option<String> {
        std::mem::take(&mut self.copy_pending)
            .then(|| self.command().map(str::to_owned))
            .flatten()
    }
}

/// The setup for the binary at `exe`: the `claude mcp add` command, its path
/// quoted for the shell when it needs it, unless `exe` is a translocated
/// copy.
fn claude_code_setup(exe: &str) -> ClaudeCodeSetup {
    if exe.contains(TRANSLOCATED) {
        return ClaudeCodeSetup::Translocated;
    }
    ClaudeCodeSetup::Command(format!(
        "claude mcp add --scope user stev -- {} {MCP_FLAG}",
        shell_quote(exe)
    ))
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

    use super::{AiTab, ClaudeCodeSetup, claude_code_setup, shell_quote};

    /// A tab whose setup offers `command`.
    fn tab_with(command: &str) -> AiTab {
        AiTab {
            setup: ClaudeCodeSetup::Command(command.to_owned()),
            ..AiTab::default()
        }
    }

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
            claude_code_setup("/Applications/Stev.app/Contents/MacOS/stev"),
            ClaudeCodeSetup::Command(
                "claude mcp add --scope user stev -- /Applications/Stev.app/Contents/MacOS/stev --mcp"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_translocated_copy_gets_no_command() {
        let exe = "/private/var/folders/xy/abc/T/AppTranslocation/0A1B2C3D/d/Stev.app/Contents/MacOS/stev";
        assert_eq!(claude_code_setup(exe), ClaudeCodeSetup::Translocated);
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
        let mut tab = tab_with("claude mcp add");
        assert_eq!(tab.take_copy(), None);
        tab.request_copy();
        assert!(tab.copied);
        assert_eq!(tab.take_copy().as_deref(), Some("claude mcp add"));
        assert_eq!(tab.take_copy(), None);
    }

    #[test]
    fn without_a_command_a_copy_puts_nothing_on_the_clipboard() {
        let mut tab = AiTab {
            setup: ClaudeCodeSetup::Translocated,
            ..AiTab::default()
        };
        tab.request_copy();
        assert_eq!(tab.take_copy(), None);
    }
}
