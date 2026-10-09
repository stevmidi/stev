//! Undoable retime of one clip together with the project tempo: Enter's
//! tempo fit to the project's only clip (`220-capture-without-pending-view.md`)
//! and `⌥=`/`⌥-`'s one-bar stretch (`010-keybindings.md`). While a project
//! has one clip, that clip *is* the tempo reference: `[`/`]` shape its exact
//! length by ear with the tempo left alone, Enter then makes that length a
//! whole number of bars by changing the tempo rather than by rounding the
//! edge, and `⌥=`/`⌥-` fix a bar count that came out wrong. Nothing about
//! either is stored: each acts on what's there when the key is pressed.

use uuid::Uuid;

use crate::metadata::clip_metadata::ClipMetadata;
use crate::models::{
    clip::{Clip, ClipBounds, EventSpaceRetime},
    event::Event,
};

use super::super::super::Sequencer;
use super::super::EditResult;

// ---------------------------------------------------------------------------
// RetimeClipEdit
// ---------------------------------------------------------------------------

/// Everything a retime changes: where the clip sits, its events and the
/// project tempo.
#[derive(Clone)]
struct RetimedState {
    /// The clip's bounds.
    bounds: ClipBounds,
    /// The clip's events.
    events: Vec<Event>,
    /// The project tempo, µs per quarter note — what a retime of the
    /// project's only clip sets (`RetimeClipEdit::sets_tempo`).
    tempo_us: i32,
    /// The clip cursor, event ticks — a retime rescales it with the events.
    cursor_tick: i32,
}

/// Which retime a [`RetimeClipEdit`] makes.
#[derive(Clone, Copy)]
enum Retime {
    /// Enter: fit the tempo so the clip's length is a whole number of bars
    /// (`Sequencer::fit_first_clip_tempo`, with its octave correction).
    FitTempo,
    /// `⌥=`/`⌥-`: stretch the clip to this many ticks — a bar longer or
    /// shorter — its notes retimed to fill it (`Sequencer::rescale_clip_tempo`).
    Rescale(i32),
}

/// Retimes one clip's events and, when it is the project's only clip, the
/// project tempo with them — Enter's fit ([`fit_sole_clip`](Self::fit_sole_clip))
/// or `⌥=`/`⌥-` ([`rescale_selected`](Self::rescale_selected)). Either way the
/// window start is then put back on a bar line
/// (`Clip::align_window_start_to_bar`). The first `edit()` computes that and
/// keeps the result, so a redo restores it exactly; `undo()` restores the
/// bounds, the events *and* the tempo together — unlike a capture commit's
/// undo, which leaves the tempo, because here the tempo is the point of the
/// edit and a clip retimed to one tempo is wrong at another.
pub(crate) struct RetimeClipEdit {
    /// Track the clip is on.
    track_idx: usize,
    /// The clip.
    clip_id: Uuid,
    /// Which retime.
    retime_kind: Retime,
    /// Whether the edit moves the project tempo: only on the project's only
    /// clip. When not, neither it nor its undo/redo touch the tempo, so a
    /// tap tempo in between survives.
    sets_tempo: bool,
    /// State before the first `edit()`.
    before: RetimedState,
    /// State after the first `edit()`, replayed by redo.
    after: Option<RetimedState>,
    /// How the first `edit()` moved the clip's event ticks; undo reports
    /// its inverse.
    retime: EventSpaceRetime,
}

impl RetimeClipEdit {
    /// Fits the tempo to the project's only clip as it stands (Enter).
    /// `None` unless there is exactly one clip, or when it is already whole
    /// bars from a bar line — nothing to fit.
    pub(crate) fn fit_sole_clip(sequencer: &Sequencer) -> Option<Self> {
        if sequencer.number_of_clips() != 1 {
            return None;
        }
        let (track_idx, clip) = sequencer
            .tracks()
            .iter()
            .enumerate()
            .find_map(|(idx, track)| track.clips().first().map(|clip| (idx, clip)))?;
        let bar = sequencer.meter().bar_ticks();
        if clip.region_length() % bar == 0 && clip.region().start() % bar == 0 {
            return None;
        }

        Some(Self::new(sequencer, track_idx, clip, Retime::FitTempo))
    }

    /// Stretches the selected clip `direction` bars longer/shorter (`⌥=`/`⌥-`).
    /// `None` without a selected clip, or when it is one bar and can't shrink.
    pub(crate) fn rescale_selected(sequencer: &Sequencer, direction: i32) -> Option<Self> {
        let track_idx = sequencer.selected_track_index()?;
        let clip = sequencer.selected_clip()?;
        let target_length =
            Sequencer::rescaled_length(clip.region_length(), direction, sequencer.meter())?;

        Some(Self::new(
            sequencer,
            track_idx,
            clip,
            Retime::Rescale(target_length),
        ))
    }

    /// The edit on `clip` (on `track_idx`), its state before taken now.
    fn new(sequencer: &Sequencer, track_idx: usize, clip: &Clip, retime_kind: Retime) -> Self {
        Self {
            track_idx,
            clip_id: clip.id(),
            retime_kind,
            sets_tempo: sequencer.number_of_clips() == 1,
            before: RetimedState {
                bounds: clip.bounds(),
                events: clip.events().to_vec(),
                tempo_us: sequencer.tempo_us(),
                cursor_tick: clip.cursor_tick(),
            },
            after: None,
            retime: EventSpaceRetime::IDENTITY,
        }
    }

    /// First call: retimes the clip (and, for the only clip, the tempo) and
    /// keeps the result. Later calls (redo): restore that result. Returns
    /// [`EditResult::ClipResized`], or `NoOp` if the clip is gone.
    pub(in crate::core::sequencer::edit) fn edit(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        if let Some(after) = &self.after {
            return self.restore(sequencer, after, self.retime);
        }

        let current_tempo = sequencer.tempo_us();
        let meter = sequencer.meter();
        let Some((after, metadata)) =
            sequencer.edit_clip_events(self.track_idx, self.clip_id, |clip| {
                let (tempo_us, retime) = match self.retime_kind {
                    Retime::FitTempo => Sequencer::fit_first_clip_tempo(clip, current_tempo, meter),
                    Retime::Rescale(target_length) => {
                        Sequencer::rescale_clip_tempo(clip, current_tempo, target_length, meter)
                    }
                }
                .unwrap_or((current_tempo, EventSpaceRetime::IDENTITY));
                self.retime = retime.then(clip.align_window_start_to_bar(meter));
                let after = RetimedState {
                    bounds: clip.bounds(),
                    events: clip.events().to_vec(),
                    tempo_us,
                    cursor_tick: clip.cursor_tick(),
                };
                (after, ClipMetadata::from_clip(self.track_idx, clip))
            })
        else {
            return EditResult::NoOp;
        };

        if self.sets_tempo {
            sequencer.set_tempo(after.tempo_us);
        }
        self.after = Some(after);
        EditResult::ClipResized {
            clip: metadata,
            retime: Some(self.retime),
        }
    }

    /// Restores the clip and tempo from before the first `edit()`.
    pub(in crate::core::sequencer::edit) fn undo(
        &mut self,
        sequencer: &mut Sequencer,
    ) -> EditResult {
        self.restore(sequencer, &self.before, self.retime.inverse())
    }

    /// Puts the clip and the tempo back to `state`, in place (the clip keeps
    /// its id and shared atomics); `retime` is how that moves its event ticks.
    /// Through [`Sequencer::edit_clip_events`], as every event retime.
    fn restore(
        &self,
        sequencer: &mut Sequencer,
        state: &RetimedState,
        retime: EventSpaceRetime,
    ) -> EditResult {
        let Some(metadata) = sequencer.edit_clip_events(self.track_idx, self.clip_id, |clip| {
            clip.set_bounds(state.bounds);
            clip.restore_events(state.events.clone());
            clip.set_cursor_tick(state.cursor_tick);
            ClipMetadata::from_clip(self.track_idx, clip)
        }) else {
            return EditResult::NoOp;
        };

        if self.sets_tempo {
            sequencer.set_tempo(state.tempo_us);
        }
        EditResult::ClipResized {
            clip: metadata,
            retime: Some(retime),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use rtrb::Consumer;
    use undo::Record;

    use crate::core::sequencer::{ClipInstrumentEvent, SequencerEdit};

    use crate::core::time::Meter;
    use crate::models::clip::{Clip, ClipEdge};

    use crate::core::sequencer::test_support::{
        clip_at, drain, instrument_track_0, note_off, note_on, sequencer_with,
    };

    use super::*;

    fn test_sequencer() -> Sequencer {
        let mut sequencer = sequencer_with(false).0;
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer
    }

    /// The project's only clip: at bar 1, a 2½-bar window starting 4 bars
    /// into its events, with a note per beat.
    fn sequencer_with_first_clip() -> (Sequencer, Uuid) {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let mut sequencer = test_sequencer();
        let mut clip = Clip::new();
        clip.region_mut()
            .set_region(Some(bar * 4), Some(bar * 4 + bar * 5 / 2));
        for beat in 0..10 {
            let tick = bar * 4 + beat * bar / 4;
            clip.add_event(Event::new(tick, 0, vec![0x90, 60, 100]));
            clip.add_event(Event::new(tick + 100, 0, vec![0x80, 60, 0]));
        }
        let clip_id = clip.id();
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.select_clip(Some(clip_id));
        (sequencer, clip_id)
    }

    fn clip(sequencer: &Sequencer) -> &Clip {
        sequencer.selected_clip().unwrap()
    }

    /// Enter on a project's only clip, 2¼ bars long: the length becomes whole
    /// bars by moving the tempo and the window lands on a bar line; one undo
    /// restores the bounds, the note timing, the cursor and the tempo
    /// together, and redo replays the fit.
    #[test]
    fn enter_fits_the_tempo_to_the_only_clip_and_undoes_with_it() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, clip_id) = sequencer_with_first_clip();
        let mut target = clip(&sequencer).bounds();
        target.region_end = target.region_start + bar * 9 / 4;
        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(clip_id)
            .unwrap()
            .set_bounds(target);

        let before_bounds = clip(&sequencer).bounds();
        let before_events: Vec<i32> = clip(&sequencer).events().iter().map(Event::tick).collect();
        let before_tempo = sequencer.tempo_us();
        let before_cursor = clip(&sequencer).cursor_tick();

        let edit = RetimeClipEdit::fit_sole_clip(&sequencer).unwrap();
        let mut record = Record::new();
        record.edit(&mut sequencer, SequencerEdit::RetimeClip(edit));

        let fitted_tempo = sequencer.tempo_us();
        let fitted_bounds = clip(&sequencer).bounds();
        assert_ne!(fitted_tempo, before_tempo, "the tempo follows the length");
        assert_eq!(clip(&sequencer).region_length() % bar, 0, "whole bars");
        assert_eq!(fitted_bounds.region_start % bar, 0, "window on a bar line");

        record.undo(&mut sequencer);
        assert_eq!(clip(&sequencer).bounds(), before_bounds);
        assert_eq!(sequencer.tempo_us(), before_tempo);
        let events: Vec<i32> = clip(&sequencer).events().iter().map(Event::tick).collect();
        assert_eq!(events, before_events, "note timing restored");
        assert_eq!(
            clip(&sequencer).cursor_tick(),
            before_cursor,
            "cursor restored"
        );

        record.redo(&mut sequencer);
        assert_eq!(clip(&sequencer).bounds(), fitted_bounds);
        assert_eq!(sequencer.tempo_us(), fitted_tempo);
    }

    /// The fit and its undo each report how the clip's event ticks moved,
    /// so the open clip view can keep the notes where they were on screen:
    /// the window start onto the new one and back.
    #[test]
    fn the_fit_and_its_undo_report_the_retime() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, clip_id) = sequencer_with_first_clip();
        let mut target = clip(&sequencer).bounds();
        target.region_end = target.region_start + bar * 9 / 4;
        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(clip_id)
            .unwrap()
            .set_bounds(target);
        let before_start = target.region_start;

        let mut edit = RetimeClipEdit::fit_sole_clip(&sequencer).unwrap();
        let EditResult::ClipResized {
            retime: Some(fit), ..
        } = edit.edit(&mut sequencer)
        else {
            panic!("a fit reports its retime");
        };
        let fitted_start = clip(&sequencer).region().start();
        assert!((fit.map_tick(before_start) - fitted_start).abs() <= 1);

        let EditResult::ClipResized {
            retime: Some(unfit),
            ..
        } = edit.undo(&mut sequencer)
        else {
            panic!("an undone fit reports its retime");
        };
        assert!((unfit.map_tick(fitted_start) - before_start).abs() <= 1);
    }

    /// Nothing to fit: a clip already whole bars from a bar line, or a
    /// project with a second clip.
    #[test]
    fn nothing_to_fit_on_a_whole_bar_clip_or_with_a_second_clip() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, clip_id) = sequencer_with_first_clip();
        let mut whole = clip(&sequencer).bounds();
        whole.region_end = whole.region_start + bar * 2;
        sequencer.tracks_mut()[0]
            .get_clip_by_id_mut(clip_id)
            .unwrap()
            .set_bounds(whole);
        assert!(
            RetimeClipEdit::fit_sole_clip(&sequencer).is_none(),
            "already whole bars"
        );

        let (mut sequencer, _) = sequencer_with_first_clip();
        let mut other = Clip::new();
        other.set_start_tick(bar * 20);
        other.region_mut().set_region(Some(0), Some(bar));
        sequencer.tracks_mut()[0].add_clip(&other);
        assert!(
            RetimeClipEdit::fit_sole_clip(&sequencer).is_none(),
            "a second clip"
        );
    }

    /// While the project has one clip, `]` ends it exactly at the cursor —
    /// the clip cursor in the clip view, the arranger cursor in the arranger:
    /// its length is what Enter fits the tempo to. With a second clip, the
    /// clip view's `]` rounds up to the bar the cursor is in.
    #[test]
    fn the_end_is_exact_with_one_clip_and_rounds_up_with_more() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, clip_id) = sequencer_with_first_clip();
        let length = bar * 2 + 123;
        let region_start = clip(&sequencer).region().start();
        sequencer.set_selected_clip_cursor_tick(region_start + length);
        sequencer.cursor_tick.store(length, Ordering::Relaxed);

        for in_clip_view in [false, true] {
            let target = sequencer
                .clip_edge_target(ClipEdge::End, in_clip_view)
                .unwrap();
            assert_eq!(target.clip_id, clip_id);
            assert_eq!(
                target.bounds.region_end - target.bounds.region_start,
                length,
                "exact (clip view: {in_clip_view})"
            );
        }

        let mut other = Clip::new();
        other.set_start_tick(bar * 20);
        other.region_mut().set_region(Some(0), Some(bar));
        sequencer.tracks_mut()[0].add_clip(&other);
        let target = sequencer.clip_edge_target(ClipEdge::End, true).unwrap();
        assert_eq!(
            target.bounds.region_end - target.bounds.region_start,
            bar * 3,
            "the bar the cursor is in becomes the last"
        );
    }

    /// A running sequencer whose only clip, 2¼ bars at bar 1 on an
    /// instrument track, holds one short note at bar 3 — Enter fits it to 2
    /// bars, pulling the note earlier by 1/9.
    fn running_with_unfitted_clip() -> (Sequencer, Consumer<ClipInstrumentEvent>) {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);
        let mut clip = clip_at(0, bar * 9 / 4);
        clip.add_event(note_on(bar * 2));
        clip.add_event(note_off(bar * 2 + bar / 8));
        sequencer.tracks_mut()[0].add_clip(&clip);
        sequencer.running.store(true, Ordering::Relaxed);
        (sequencer, plugin_rx)
    }

    /// Regression: Enter while a note sounds must release it when the fit
    /// moves it out from under the playhead (it now ends before it).
    #[test]
    fn the_fit_releases_a_note_it_moves_out_from_under_the_playhead() {
        let (mut sequencer, mut plugin_rx) = running_with_unfitted_clip();
        sequencer.reset_to_tick(Meter::FOUR_FOUR.bars_to_ticks(2));
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        let mut edit = RetimeClipEdit::fit_sole_clip(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }

    /// Regression: undoing the fit swaps the whole event list back; a note
    /// sounding at the playhead that the swap moves later must be released,
    /// not left hanging.
    #[test]
    fn undoing_the_fit_releases_a_note_it_moves_out_from_under_the_playhead() {
        let (mut sequencer, mut plugin_rx) = running_with_unfitted_clip();
        let mut edit = RetimeClipEdit::fit_sole_clip(&sequencer).unwrap();
        edit.edit(&mut sequencer);
        let fitted_on = sequencer.tracks()[0].clips()[0].events()[0].tick();
        sequencer.reset_to_tick(fitted_on);
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        edit.undo(&mut sequencer);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }

    /// Adds `clip` to track 0 of `sequencer` and selects it.
    fn add_selected(sequencer: &mut Sequencer, clip: &Clip) {
        sequencer.tracks_mut()[0].add_clip(clip);
        let track_id = sequencer.track_id_by_index(0).unwrap();
        sequencer.select_track(Some(track_id));
        sequencer.select_clip(Some(clip.id()));
    }

    /// Records `⌥=`/`⌥-` (`direction`) on the selected clip.
    fn rescale(sequencer: &mut Sequencer, record: &mut Record<SequencerEdit>, direction: i32) {
        let edit = RetimeClipEdit::rescale_selected(sequencer, direction).unwrap();
        record.edit(sequencer, SequencerEdit::RetimeClip(edit));
    }

    /// `⌥-` on the project's only clip moves the tempo with it; one undo
    /// restores the bounds, the note timing and the tempo together, and redo
    /// replays it.
    #[test]
    fn rescale_moves_the_tempo_and_undoes_with_it() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, _) = sequencer_with(false);
        let mut take = clip_at(0, bar * 2);
        take.add_event(note_on(bar));
        take.add_event(note_off(bar + bar / 2));
        add_selected(&mut sequencer, &take);
        let before_bounds = clip(&sequencer).bounds();
        let before_events: Vec<i32> = clip(&sequencer).events().iter().map(Event::tick).collect();
        let before_tempo = sequencer.tempo_us();

        let mut record = Record::new();
        rescale(&mut sequencer, &mut record, -1);
        let rescaled_tempo = sequencer.tempo_us();
        assert_eq!(clip(&sequencer).region_length(), bar);
        assert!(
            (rescaled_tempo - before_tempo * 2).abs() <= 1,
            "the same music in half the bars: half the BPM"
        );

        record.undo(&mut sequencer);
        assert_eq!(clip(&sequencer).bounds(), before_bounds);
        assert_eq!(
            sequencer.tempo_us(),
            before_tempo,
            "the tempo is undone too"
        );
        let events: Vec<i32> = clip(&sequencer).events().iter().map(Event::tick).collect();
        assert_eq!(events, before_events, "note timing restored");

        record.redo(&mut sequencer);
        assert_eq!(clip(&sequencer).region_length(), bar);
        assert_eq!(sequencer.tempo_us(), rescaled_tempo);
    }

    /// With a second clip, `⌥=` retimes the selected one against the tempo
    /// and leaves the tempo alone, through the edit, its undo and redo — a
    /// tap tempo set in between is kept; a one-bar
    /// clip can't shrink, so `⌥-` on it is no edit.
    #[test]
    fn rescale_leaves_the_tempo_with_a_second_clip_and_skips_a_one_bar_shrink() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, _) = sequencer_with(false);
        let mut other = clip_at(0, bar);
        other.set_start_tick(bar * 20);
        sequencer.tracks_mut()[0].add_clip(&other);
        add_selected(&mut sequencer, &clip_at(0, bar));
        let tempo = sequencer.tempo_us();
        assert!(RetimeClipEdit::rescale_selected(&sequencer, -1).is_none());

        let mut record = Record::new();
        rescale(&mut sequencer, &mut record, 1);
        assert_eq!(clip(&sequencer).region_length(), bar * 2);
        assert_eq!(sequencer.tempo_us(), tempo);

        // A tap tempo in between (not an edit) survives the undo and redo.
        let tapped = tempo + 1000;
        sequencer.set_tempo(tapped);
        record.undo(&mut sequencer);
        assert_eq!(clip(&sequencer).region_length(), bar);
        assert_eq!(sequencer.tempo_us(), tapped);
        record.redo(&mut sequencer);
        assert_eq!(sequencer.tempo_us(), tapped);
    }

    /// Regression: a `⌥-` rescale while a note sounds must release it when
    /// the retime takes it out from under the playhead — the halved note now
    /// ends before the playhead, where the walk would never meet its off.
    #[test]
    fn rescale_releases_a_note_it_moves_out_from_under_the_playhead() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, mut plugin_rx) = sequencer_with(true);
        instrument_track_0(&mut sequencer);
        let mut clip = clip_at(0, bar * 2);
        clip.add_event(note_on(bar));
        clip.add_event(note_off(bar + bar / 2));
        add_selected(&mut sequencer, &clip);

        sequencer.running.store(true, Ordering::Relaxed);
        sequencer.reset_to_tick(bar);
        sequencer.tick(Instant::now());
        assert_eq!(drain(&mut plugin_rx), vec![[0x90, 60, 100]]);

        rescale(&mut sequencer, &mut Record::new(), -1);
        sequencer.tick(Instant::now());

        assert_eq!(drain(&mut plugin_rx), vec![[0x80, 60, 0]]);
    }

    /// Regression: `⌥-` on a fitted first clip whose window starts on an odd
    /// bar of its event space halved the start off the bar line, so a
    /// following Enter — with nothing left to fit — still shifted the event
    /// space. The rescale keeps the window on a bar line; Enter has nothing
    /// to do.
    #[test]
    fn rescale_keeps_the_window_on_a_bar_line_so_enter_has_nothing_to_fit() {
        let bar = Meter::FOUR_FOUR.bar_ticks();
        let (mut sequencer, _) = sequencer_with(false);
        let mut fitted = Clip::new();
        fitted.region_mut().set_region(Some(bar * 3), Some(bar * 5));
        fitted.add_event(note_on(bar * 3));
        fitted.add_event(note_off(bar * 4));
        add_selected(&mut sequencer, &fitted);

        rescale(&mut sequencer, &mut Record::new(), -1);

        assert_eq!(clip(&sequencer).region_length(), bar);
        assert_eq!(
            clip(&sequencer).region().start() % bar,
            0,
            "window on a bar line"
        );
        assert!(RetimeClipEdit::fit_sole_clip(&sequencer).is_none());
    }
}
