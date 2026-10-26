//! The footer's passing message: what an action just did, or why it did
//! nothing ("Exported …mid", "Select a single clip to export"), held then
//! faded. Arrives as `UiEvent::Status`, lives in `RenderState::status`, drawn
//! by `draw_status_bar`. See `030-ui-design.md` § Header & Footer. Also the
//! running audio export's progress line, shown under no passing message.

use std::time::{Duration, Instant};

/// A passing message in the footer — the result of an action ("Exported
/// …mid", or why nothing happened). One at a time: a new one replaces it. Shown
/// for [`HOLD`](Self::HOLD), then faded out over [`FADE`](Self::FADE). See
/// `030-ui-design.md` § Header & Footer.
pub(super) struct StatusMessage {
    /// What it says.
    pub(super) text: String,
    /// When it arrived.
    pub(super) shown_at: Instant,
}

impl StatusMessage {
    /// How long the message shows at full strength.
    const HOLD: Duration = Duration::from_millis(3000);
    /// How long it then takes to fade out.
    const FADE: Duration = Duration::from_millis(500);

    /// A message arriving now.
    pub(super) fn new(text: String) -> Self {
        StatusMessage {
            text,
            shown_at: Instant::now(),
        }
    }

    /// Whether the message outranks a standing question asked at `asked_at`
    /// (`None`: none asked): the newer of the two has the footer, so a
    /// message still held from the last action never hides a fresh
    /// question.
    pub(super) fn outranks(&self, asked_at: Option<Instant>) -> bool {
        asked_at.is_none_or(|asked_at| self.shown_at > asked_at)
    }

    /// The fade's frame interval: the only stretch that needs a steady
    /// cadence.
    const FADE_FRAME: Duration = Duration::from_millis(33);

    /// The message's opacity at `now`, `1.0` while held, falling to `0.0`
    /// across the fade; `None` once it is gone.
    pub(super) fn opacity(&self, now: Instant) -> Option<f32> {
        let elapsed = now.saturating_duration_since(self.shown_at);
        let Some(fading) = elapsed.checked_sub(Self::HOLD) else {
            return Some(1.0);
        };
        let fading = fading.as_secs_f32() / Self::FADE.as_secs_f32();
        (fading < 1.0).then_some(1.0 - fading)
    }

    /// How long until the message next looks different at `now`: the end of
    /// the hold (one wake, the screen is unchanged until then), a fade frame,
    /// or `None` once it is gone.
    pub(super) fn next_repaint(&self, now: Instant) -> Option<Duration> {
        self.opacity(now)?;
        let elapsed = now.saturating_duration_since(self.shown_at);
        Some(
            Self::HOLD
                .checked_sub(elapsed)
                .filter(|left| !left.is_zero())
                .unwrap_or(Self::FADE_FRAME),
        )
    }
}

/// The footer's line while an audio export runs: how much of it is written,
/// of how much, and how to stop it — `Exporting… 0:42 / 2:10 — Esc cancels`.
pub(super) fn export_progress_text(written_ms: u32, total_ms: u32) -> String {
    let (written_m, written_s) = minutes_seconds(written_ms.min(total_ms));
    let (total_m, total_s) = minutes_seconds(total_ms);
    format!("Exporting… {written_m}:{written_s:02} / {total_m}:{total_s:02} — Esc cancels")
}

/// `ms` as whole minutes and seconds, the minutes as many as it takes.
fn minutes_seconds(ms: u32) -> (u32, u32) {
    let seconds = ms / 1000;
    (seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{StatusMessage, export_progress_text};

    #[test]
    fn export_progress_reads_in_minutes_and_seconds() {
        assert_eq!(
            export_progress_text(42_900, 130_000),
            "Exporting… 0:42 / 2:10 — Esc cancels"
        );
        assert_eq!(
            export_progress_text(0, 3_725_000),
            "Exporting… 0:00 / 62:05 — Esc cancels"
        );
        // The files run a block past the selection; the line doesn't.
        assert_eq!(
            export_progress_text(5_010, 5_000),
            "Exporting… 0:05 / 0:05 — Esc cancels"
        );
    }

    #[test]
    fn the_newer_of_a_message_and_a_question_has_the_footer() {
        let message = StatusMessage::new("Deleted Jams".to_owned());
        assert!(message.outranks(None));
        assert!(message.outranks(Some(message.shown_at - Duration::from_millis(1))));
        assert!(!message.outranks(Some(message.shown_at + Duration::from_millis(1))));
    }

    #[test]
    fn a_status_message_holds_then_fades_then_is_gone() {
        let (hold, fade) = (StatusMessage::HOLD, StatusMessage::FADE);
        let message = StatusMessage::new("Exported".to_owned());
        let at = |elapsed: Duration| message.shown_at + elapsed;

        assert_eq!(message.opacity(at(Duration::ZERO)), Some(1.0));
        assert_eq!(
            message.opacity(at(hold - Duration::from_millis(1))),
            Some(1.0)
        );
        let halfway = message.opacity(at(hold + fade / 2)).unwrap();
        assert!((halfway - 0.5).abs() < 1e-3);
        assert_eq!(message.opacity(at(hold + fade)), None);
    }

    #[test]
    fn a_status_message_wakes_once_for_the_hold_then_every_fade_frame() {
        let (hold, fade) = (StatusMessage::HOLD, StatusMessage::FADE);
        let message = StatusMessage::new("Exported".to_owned());
        let at = |elapsed: Duration| message.shown_at + elapsed;

        assert_eq!(
            message.next_repaint(at(Duration::from_millis(1000))),
            Some(hold - Duration::from_millis(1000))
        );
        assert_eq!(
            message.next_repaint(at(hold)),
            Some(StatusMessage::FADE_FRAME)
        );
        assert_eq!(message.next_repaint(at(hold + fade)), None);
    }
}
