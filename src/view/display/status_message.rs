//! The footer's passing message: what an action just did, or why it did
//! nothing ("Exported …mid", "Select a single clip to export"), held then
//! faded. Arrives as `UiEvent::Status`, lives in `RenderState::status`, drawn
//! by `draw_status_bar`. See `030-ui-design.md` § Header & Footer.

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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::StatusMessage;

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
