//! The one enum naming which screen the app is showing.
//!
//! Stored in [`SharedAtomics::view_state`](crate::core::shared_atomics::SharedAtomics)
//! as an [`AtomicU8`](std::sync::atomic::AtomicU8) — session state, never
//! persisted — so both the `"sequencer"` thread and the render thread can read
//! the current view without a lock. Handlers and `Display` convert through
//! [`ViewState::from_u8`]. The transition rules (what each view layers over,
//! which keys move between them, what "reserved span" and modal mean) are in
//! `020-views-and-state.md`; `030-ui-design.md` covers how each one is drawn.

/// One of the two timeline surfaces sharing the lane area: the arranger and
/// the clip view (piano roll). Keyboard focus is the [`ViewState`]
/// (`Arranger` / `Clip`); a `Pane` names a surface independently of focus —
/// which one a click landed in, or which one a draw pass is for. The layout
/// is `view::display::pane`; see `archive/210-docked-clip-panel.md`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pane {
    /// The arranger: tracks, clips, the performance lane.
    Arranger,
    /// The clip view: the piano roll of one clip.
    Clip,
}

/// Which view is on screen: the arranger or the clip view (piano roll). The
/// settings modal is not a view: it is an overlay `Display` owns and draws
/// over whichever view is on screen, which stays its value meanwhile — so
/// "is the clip view open?" always has a true answer. Whether notes are selected
/// in `Clip` is not a view of its own: it is
/// [`SharedAtomics::has_event_selection`](crate::core::shared_atomics::SharedAtomics),
/// a mirror of the open clip's event selection (there used to be a separate
/// `ClipEdit` view for it; see `020-views-and-state.md`).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
#[repr(u8)]
pub enum ViewState {
    /// The clip arrangement timeline — the home view.
    #[default]
    Arranger = 0,
    /// A single clip's piano roll — navigation, and editing whatever notes
    /// are selected.
    Clip = 1,
}

impl ViewState {
    /// Reads the view back from the `u8` in the shared atomic. Unknown values
    /// fall back to [`Arranger`](Self::Arranger); discriminants are kept
    /// contiguous so that never happens at runtime (see `080-conventions.md`).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ViewState::Clip,
            _ => ViewState::Arranger,
        }
    }

    /// True for the piano-roll view ([`Clip`](Self::Clip)) — the one that
    /// draws a single clip's events rather than the arrangement.
    pub fn is_clip_view(self) -> bool {
        self == ViewState::Clip
    }
}

#[cfg(test)]
mod tests {
    use super::ViewState;

    #[test]
    fn every_view_round_trips_through_its_discriminant() {
        for view in [ViewState::Arranger, ViewState::Clip] {
            assert_eq!(ViewState::from_u8(view as u8), view);
        }
    }

    #[test]
    fn an_unknown_discriminant_falls_back_to_the_arranger() {
        assert_eq!(ViewState::from_u8(2), ViewState::Arranger);
        assert_eq!(ViewState::from_u8(u8::MAX), ViewState::Arranger);
    }
}
