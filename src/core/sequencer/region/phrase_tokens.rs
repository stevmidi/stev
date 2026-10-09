//! Phrase-token detection — where the musician's take divides into musical
//! phrases.
//!
//! The stopped `/` snaps the start of the clip or insert it frames to one of
//! them. This module derives them from the last
//! [`PHRASE_DETECTION_WINDOW_BARS`]
//! of the take: it takes the `NoteOn` stream, measures the silence between
//! consecutive note *pairs* (not just onsets), and treats a boundary as real
//! when either the silence is clearly long on its own, or a subtler gap has
//! musical support — landing on a strong beat/bar, or bracketing a
//! plausible-length phrase. [`phrase_token_starts`](Sequencer::phrase_token_starts)
//! is the main entry. See `040-phrase-detection.md`.
//!
//! All the scoring thresholds are small named `fn`s rather than consts so they
//! can be expressed in musical units (`beats_to_ticks`, …).

use crate::{
    core::{
        config::{PHRASE_DETECTION_WINDOW_BARS, PHRASE_LAST_TOKEN_REFINE_MIN_BARS},
        time::{self, Meter},
    },
    models::{
        clip::Clip,
        event::{Event, EventType},
    },
};

use super::Sequencer;

/// A possible phrase boundary and the evidence for it: the silence before it,
/// and the take's median inter-note silence for comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhraseBoundaryCandidate {
    /// Onset tick of the note that would start the new phrase.
    tick: i32,
    /// Silence before this note, in ticks (an amount).
    silence_ticks: i32,
    /// The take's median inter-note silence, for scoring context.
    median_silence_ticks: i32,
}

impl Sequencer {
    /// The clip's `NoteOn` events, sorted by tick and deduplicated to one per
    /// tick (a chord counts once).
    fn all_note_on_events(clip: &Clip) -> Vec<Event> {
        let mut events: Vec<Event> = clip
            .events()
            .iter()
            .filter(|e| e.event_type() == Some(EventType::NoteOn))
            .cloned()
            .collect();
        events.sort_unstable_by_key(|event| event.tick());
        events.dedup_by_key(|event| event.tick());
        events
    }

    /// End of the phrase-analysis window: the latest `NoteOn` end tick in the clip.
    fn phrase_token_window_end_tick(clip: &Clip) -> i32 {
        clip.events()
            .iter()
            .filter(|event| event.event_type() == Some(EventType::NoteOn))
            .map(Event::end_tick)
            .max()
            .unwrap_or(0)
    }

    /// Start of the phrase-analysis window: `PHRASE_DETECTION_WINDOW_BARS` before the window end, floored at 0.
    fn phrase_token_window_start_tick(clip: &Clip, meter: Meter) -> i32 {
        let window_ticks = meter.bars_to_ticks(PHRASE_DETECTION_WINDOW_BARS);
        (Self::phrase_token_window_end_tick(clip) - window_ticks).max(0)
    }

    /// The `NoteOn` events inside the phrase-analysis window, anchored to the capture-buffer end so a held final note can't drag older material in.
    fn note_on_events_in_window(clip: &Clip, meter: Meter) -> Vec<Event> {
        // Anchor the analysis window to the actual capture-buffer end so a held
        // final note does not drag older phrase material back into tokenization.
        let window_start = Self::phrase_token_window_start_tick(clip, meter);

        Self::all_note_on_events(clip)
            .into_iter()
            .filter(|event| event.tick() >= window_start)
            .collect()
    }

    /// Returns the deduplicated, sorted NoteOn ticks within the last
    /// `PHRASE_DETECTION_WINDOW_BARS` of the clip. Shared by phrase-token
    /// detection and `region_start` snap.
    fn note_on_ticks_in_window(clip: &Clip, meter: Meter) -> Vec<i32> {
        Self::note_on_events_in_window(clip, meter)
            .into_iter()
            .map(|event| event.tick())
            .collect()
    }

    /// Gap between two consecutive notes, measured end-of-`previous` to onset-of-`next` (never negative).
    fn silence_ticks_between(previous: &Event, next: &Event) -> i32 {
        (next.tick() - previous.end_tick()).max(0)
    }

    // The last-N-bars analysis window can start in the middle of a token. When
    // the window edge lands directly on the first visible NoteOn, walk backward
    // through earlier NoteOns until a qualifying gap is found. If there is
    // already silence between the window edge and the first visible NoteOn,
    // treat that first visible note as the buffer token start and do not pull
    // older off-screen material back in.
    /// Walk earlier `NoteOn`s until a qualifying gap is found, so a window edge landing mid-token reports the token's true start; if there is already silence at the edge, keep the first visible note.
    fn backtrack_token_start(clip: &Clip, first_tick_in_window: i32, window_start: i32) -> i32 {
        if first_tick_in_window > window_start {
            return first_tick_in_window;
        }

        let all_note_on_events = Self::all_note_on_events(clip);

        let Some(first_idx) = all_note_on_events
            .iter()
            .position(|event| event.tick() == first_tick_in_window)
        else {
            return first_tick_in_window;
        };

        let mut token_start = first_tick_in_window;
        for idx in (1..=first_idx).rev() {
            let current = &all_note_on_events[idx];
            let previous = &all_note_on_events[idx - 1];
            if Self::silence_ticks_between(previous, current) >= Self::phrase_absolute_gap_ticks() {
                break;
            }
            token_start = previous.tick();
        }

        token_start
    }

    /// Silence long enough to be a phrase boundary on its own, no musical support needed.
    fn phrase_absolute_gap_ticks() -> i32 {
        time::beats_to_ticks(1.5)
    }

    /// Shortest span a token must cover for the snap to reach back to it:
    /// clearly longer than one bar. The bar follows the meter; the margin is
    /// a quarter note in every meter, like the module's other silence and
    /// span thresholds. It is a stretch of time, not a metric position, and
    /// a quarter is a fixed slice of it at a given tempo. The counted beat
    /// would shrink it to an eighth in x/8, where the felt pulse (6/8's
    /// dotted quarter) is slower still.
    fn phrase_min_token_length_ticks(meter: Meter) -> i32 {
        meter.bar_ticks() + time::beats_to_ticks(1.0)
    }

    /// `PHRASE_LAST_TOKEN_REFINE_MIN_BARS` in ticks — the last token must be at least this long before the extra split pass runs.
    fn phrase_last_token_refine_min_ticks(meter: Meter) -> i32 {
        meter.bars_to_ticks(PHRASE_LAST_TOKEN_REFINE_MIN_BARS)
    }

    /// Total score at which a boundary candidate is accepted.
    fn phrase_boundary_accept_score() -> i32 {
        60
    }

    /// Score above which a boundary is treated as unambiguous.
    fn phrase_boundary_strong_score() -> i32 {
        75
    }

    /// Minimum plausible phrase span for the span-shape score.
    fn phrase_boundary_min_span_ticks() -> i32 {
        time::beats_to_ticks(1.0)
    }

    /// How close to a beat/bar line still counts as landing on it.
    fn metric_tolerance_ticks() -> i32 {
        time::sixteenth_triplet_ticks()
    }

    /// Every inter-note gap in `events` as a scorable [`PhraseBoundaryCandidate`].
    fn phrase_boundary_candidates(events: &[Event]) -> Vec<PhraseBoundaryCandidate> {
        let Some(median_silence_ticks) = Self::median_silence_ticks(events) else {
            return Vec::new();
        };

        events
            .windows(2)
            .filter_map(|window| {
                let silence_ticks = Self::silence_ticks_between(&window[0], &window[1]);
                (silence_ticks > 0).then_some(PhraseBoundaryCandidate {
                    tick: window[1].tick(),
                    silence_ticks,
                    median_silence_ticks,
                })
            })
            .collect()
    }

    /// Median gap between consecutive notes in `events`, or `None` if there are fewer than two.
    fn median_silence_ticks(events: &[Event]) -> Option<i32> {
        let mut silences: Vec<i32> = events
            .windows(2)
            .map(|window| Self::silence_ticks_between(&window[0], &window[1]))
            .filter(|&s| s > 0)
            .collect();
        if silences.is_empty() {
            return None;
        }

        silences.sort_unstable();
        Some(silences[silences.len() / 2])
    }

    /// Silence that stands out against a given median — the bar a subtle gap must clear.
    fn prominent_silence_threshold(median_silence_ticks: i32) -> i32 {
        (median_silence_ticks * 5 + 1) / 2
    }

    /// Combined score for a candidate: silence + metric position + span shape, relative to `token_start`.
    fn phrase_boundary_score(
        candidate: PhraseBoundaryCandidate,
        token_start: i32,
        meter: Meter,
    ) -> i32 {
        Self::silence_boundary_score(candidate)
            + Self::metric_boundary_score(candidate.tick, meter)
            + Self::span_boundary_score(candidate.tick - token_start, meter)
    }

    /// Score contribution from how long the silence before the candidate is.
    fn silence_boundary_score(candidate: PhraseBoundaryCandidate) -> i32 {
        let absolute_gap_ticks = Self::phrase_absolute_gap_ticks();
        if candidate.silence_ticks >= absolute_gap_ticks {
            return 70;
        }

        if candidate.median_silence_ticks <= 0 {
            return 0;
        }

        let prominent_threshold = Self::prominent_silence_threshold(candidate.median_silence_ticks);
        if candidate.silence_ticks >= prominent_threshold * 2 {
            65
        } else if candidate.silence_ticks >= prominent_threshold {
            35
        } else if candidate.silence_ticks * 100 >= candidate.median_silence_ticks * 175 {
            15
        } else {
            0
        }
    }

    /// Score contribution from how near the candidate sits to a beat or bar line.
    fn metric_boundary_score(tick: i32, meter: Meter) -> i32 {
        let bar_ticks = meter.bar_ticks();
        let beat_ticks = time::beats_to_ticks(1.0);
        let half_bar_ticks = bar_ticks / 2;
        let tolerance_ticks = Self::metric_tolerance_ticks();

        if Self::distance_to_grid_ticks(tick, bar_ticks) <= tolerance_ticks {
            25
        } else if Self::distance_to_grid_ticks(tick, half_bar_ticks) <= tolerance_ticks {
            18
        } else if Self::distance_to_grid_ticks(tick, beat_ticks) <= tolerance_ticks {
            10
        } else {
            0
        }
    }

    /// Ticks from `tick` to the nearest multiple of `grid_ticks`.
    fn distance_to_grid_ticks(tick: i32, grid_ticks: i32) -> i32 {
        if grid_ticks <= 0 {
            return i32::MAX;
        }

        let position = tick.rem_euclid(grid_ticks);
        position.min(grid_ticks - position)
    }

    /// Score contribution from whether the resulting phrase span is a plausible length.
    fn span_boundary_score(span_ticks: i32, meter: Meter) -> i32 {
        if span_ticks < Self::phrase_boundary_min_span_ticks() {
            return -30;
        }

        let bar_ticks = meter.bar_ticks();
        let half_bar_ticks = bar_ticks / 2;
        let tolerance_ticks = Self::metric_tolerance_ticks();
        let preferred_spans = [bar_ticks, bar_ticks * 2, bar_ticks * 4];

        if preferred_spans
            .iter()
            .any(|&preferred_span| (span_ticks - preferred_span).abs() <= tolerance_ticks)
        {
            20
        } else if (span_ticks - half_bar_ticks).abs() <= tolerance_ticks || span_ticks >= bar_ticks
        {
            10
        } else if span_ticks >= half_bar_ticks {
            5
        } else {
            0
        }
    }

    /// Whether a candidate's combined score clears [`phrase_boundary_accept_score`](Self::phrase_boundary_accept_score).
    fn phrase_boundary_is_accepted(
        candidate: PhraseBoundaryCandidate,
        token_start: i32,
        meter: Meter,
    ) -> bool {
        let score = Self::phrase_boundary_score(candidate, token_start, meter);
        if score < Self::phrase_boundary_accept_score() {
            return false;
        }

        let span_ticks = candidate.tick - token_start;
        span_ticks >= Self::phrase_boundary_min_span_ticks()
            || score >= Self::phrase_boundary_strong_score()
    }

    /// Token-start ticks for `note_on_events`: the first note, then each accepted boundary.
    fn phrase_token_starts_from_events(
        clip: &Clip,
        note_on_events: &[Event],
        meter: Meter,
    ) -> Vec<i32> {
        let Some(first) = note_on_events.first() else {
            return Vec::new();
        };

        let window_start = Self::phrase_token_window_start_tick(clip, meter);
        let first_start = Self::backtrack_token_start(clip, first.tick(), window_start);
        let mut token_starts = vec![first_start];
        let mut token_start = first_start;

        for candidate in Self::phrase_boundary_candidates(note_on_events) {
            if candidate.tick > token_start
                && Self::phrase_boundary_is_accepted(candidate, token_start, meter)
            {
                token_starts.push(candidate.tick);
                token_start = candidate.tick;
            }
        }

        token_starts
    }

    /// If the final token is long enough, the best extra split point inside it, else `None`.
    fn refine_last_phrase_token_start(
        clip: &Clip,
        token_starts: &[i32],
        meter: Meter,
    ) -> Option<i32> {
        let &last_token_start = token_starts.last()?;
        let last_token_events: Vec<Event> = Self::all_note_on_events(clip)
            .into_iter()
            .filter(|event| event.tick() >= last_token_start)
            .collect();

        if last_token_events.len() < 3 {
            return None;
        }

        let last_token_span = last_token_events.last()?.tick() - last_token_start;
        if last_token_span < Self::phrase_last_token_refine_min_ticks(meter) {
            return None;
        }

        Self::phrase_boundary_candidates(&last_token_events)
            .into_iter()
            .rev()
            .find(|&candidate| {
                Self::phrase_boundary_is_accepted(candidate, last_token_start, meter)
            })
            .map(|candidate| candidate.tick)
    }

    /// Appends [`refine_last_phrase_token_start`](Self::refine_last_phrase_token_start)'s split to `token_starts` when the last token qualifies.
    fn refine_last_phrase_token_if_needed(
        clip: &Clip,
        mut token_starts: Vec<i32>,
        meter: Meter,
    ) -> Vec<i32> {
        if let Some(refined_last_token_start) =
            Self::refine_last_phrase_token_start(clip, &token_starts, meter)
            && token_starts.last().copied() != Some(refined_last_token_start)
        {
            token_starts.push(refined_last_token_start);
        }

        token_starts
    }

    /// Returns the first NoteOn tick of each detected phrase token touching the
    /// last `PHRASE_DETECTION_WINDOW_BARS` of the clip, in ascending order. If the
    /// analysis window starts inside a token, the first element is backtracked
    /// to that token's true start. Subsequent elements are accepted
    /// phrase-boundary candidates scored from note-pair silence, metric
    /// position, and plausible phrase span.
    ///
    /// A clear silence can still stand on its own, while subtler gaps need
    /// musical support from strong beat/bar placement or phrase-span shape.
    /// When the final detected token spans at least
    /// `PHRASE_LAST_TOKEN_REFINE_MIN_BARS`, it gets one extra scoring pass
    /// so an obviously merged final phrase can split once more without making
    /// earlier tokenization more aggressive.
    ///
    /// With no accepted boundaries, returns a single token start or an empty
    /// vector when the clip has no NoteOn events.
    pub(in crate::core::sequencer) fn phrase_token_starts(clip: &Clip, meter: Meter) -> Vec<i32> {
        let note_on_events = Self::note_on_events_in_window(clip, meter);
        let token_starts = Self::phrase_token_starts_from_events(clip, &note_on_events, meter);

        Self::refine_last_phrase_token_if_needed(clip, token_starts, meter)
    }

    /// Where a new clip framed from `capture` (sorted, lengths calculated,
    /// trimmed to the buffer) starts, and the raw window end: the raw window
    /// of `loop_reference_length` ending at the last note, its start snapped
    /// to a phrase start ([`snap_region_start_to_note_on`](Self::snap_region_start_to_note_on)).
    /// The one place the stopped `/` picks the
    /// start, and what the capture fixtures record.
    pub(in crate::core::sequencer) fn detected_phrase_window(
        capture: &Clip,
        loop_reference_length: i32,
        meter: Meter,
    ) -> (i32, i32) {
        Self::snapped_window_from_last(capture, EventType::NoteOn, loop_reference_length, meter)
    }

    /// The `(start, end)` window of `length` ending at `capture`'s last event
    /// of `event_type`, its start snapped to a phrase start
    /// ([`snap_region_start_to_note_on`](Self::snap_region_start_to_note_on)).
    /// Shared by the stopped `/`'s new-clip and insert windows.
    pub(in crate::core::sequencer) fn snapped_window_from_last(
        capture: &Clip,
        event_type: EventType,
        length: i32,
        meter: Meter,
    ) -> (i32, i32) {
        let (start, end) =
            Self::calculate_phrase_window_from_last_note(capture, event_type, length);
        (
            Self::snap_region_start_to_note_on(capture, start, meter),
            end,
        )
    }

    /// Snaps `region_start` to the selected phrase-token start within the last
    /// `PHRASE_DETECTION_WINDOW_BARS` of the clip. Splits the window into phrase
    /// tokens via [`Self::phrase_token_starts`] and prefers the first NoteOn of the
    /// latest usable token. If the last token consists only of the final NoteOn (a
    /// trailing downbeat end-marker the musician played to mark phrase end),
    /// the snap removes that marker token first.
    ///
    /// When multiple tokens remain, a trailing single-note marker token is
    /// removed first. If the remaining latest token is only a tiny trailing
    /// fragment (fewer than three note-ons and shorter than one beat to the
    /// capture end), step back once more to the previous token. If the latest
    /// remaining token still sits at or after the raw `region_start`, keep that
    /// latest token. Otherwise, the chooser falls back to the latest eligible
    /// earlier token. Single-token windows keep their first token instead of
    /// trimming inward to the first note at or after the raw start. Returns
    /// `region_start` unchanged only when no NoteOn events are present.
    pub(in crate::core::sequencer) fn snap_region_start_to_note_on(
        clip: &Clip,
        region_start: i32,
        meter: Meter,
    ) -> i32 {
        let note_on_ticks = Self::note_on_ticks_in_window(clip, meter);
        let Some(&last_tick) = note_on_ticks.last() else {
            return region_start;
        };

        // Never empty here: the window's first note-on opens a token.
        let starts = Self::phrase_token_starts(clip, meter);
        let mut candidates = match starts.as_slice() {
            [] => return region_start,
            [only] => return *only,
            _ => starts.clone(),
        };

        // `starts` has at least two entries, so each pop below leaves one.
        if candidates.last() == Some(&last_tick) {
            candidates.pop();
        }

        if candidates.len() >= 2 {
            let latest_candidate = candidates[candidates.len() - 1];
            let latest_token_note_on_count = note_on_ticks
                .iter()
                .copied()
                .filter(|&tick| tick >= latest_candidate)
                .count();

            if latest_token_note_on_count < 3
                && last_tick - latest_candidate < time::beats_to_ticks(1.0)
            {
                candidates.pop();
            }
        }

        let latest_candidate = candidates[candidates.len() - 1];

        if latest_candidate >= region_start {
            return latest_candidate;
        }

        // Every candidate starts before `region_start` here: reach back to the
        // latest one long enough to be a phrase.
        candidates
            .iter()
            .rev()
            .copied()
            .find(|&start| {
                let next_boundary = starts
                    .iter()
                    .copied()
                    .find(|&next_start| next_start > start)
                    .unwrap_or(last_tick);
                next_boundary - start >= Self::phrase_min_token_length_ticks(meter)
            })
            .unwrap_or(latest_candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sequencer::test_support::{note_off, note_on};

    fn clip_with_note_ons(ticks: &[i32]) -> Clip {
        let mut clip = Clip::new();
        for &t in ticks {
            clip.add_event(note_on(t));
        }
        clip
    }

    fn clip_with_note_pairs(pairs: &[(i32, i32)]) -> Clip {
        let mut clip = Clip::new();
        for &(start, end) in pairs {
            clip.add_event(note_on(start));
            clip.add_event(note_off(end));
        }
        clip.sort_events_by_tick();
        clip.calculate_note_lengths();
        clip
    }

    const BAR: i32 = 3840;

    /// A boundary on a 3/4 bar line scores as a bar line in 3/4, but only as
    /// a beat in 4/4.
    #[test]
    fn metric_score_reads_bar_lines_of_the_meter() {
        let three_four = Meter::new(3, 4).unwrap();
        let tick = three_four.bar_ticks();
        assert_eq!(Sequencer::metric_boundary_score(tick, three_four), 25);
        assert_eq!(Sequencer::metric_boundary_score(tick, Meter::FOUR_FOUR), 10);
    }

    /// A phrase one 6/8 bar long is a preferred span in 6/8, not in 4/4.
    #[test]
    fn span_score_prefers_whole_bars_of_the_meter() {
        let six_eight = Meter::new(6, 8).unwrap();
        let span = six_eight.bar_ticks();
        assert_eq!(Sequencer::span_boundary_score(span, six_eight), 20);
        assert!(Sequencer::span_boundary_score(span, Meter::FOUR_FOUR) < 20);
    }

    #[test]
    fn snap_uses_first_note_of_last_token() {
        // Three tokens separated by ≥ 1.5-beat gaps. Should snap to the first note of
        // the last token.
        let token_a = vec![0, 240, 480];
        let token_b_start = 480 + Sequencer::phrase_absolute_gap_ticks();
        let token_b = vec![token_b_start, token_b_start + 240];
        let token_c_start = token_b_start + 240 + Sequencer::phrase_absolute_gap_ticks();
        let token_c = vec![token_c_start, token_c_start + 240, token_c_start + 480];
        let ticks: Vec<i32> = token_a.into_iter().chain(token_b).chain(token_c).collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_eq!(result, token_c_start);
    }

    #[test]
    fn snap_skips_trailing_downbeat_marker_to_previous_token() {
        // A phrase token followed by ≥ 1 beat gap and a single downbeat end-marker.
        // The last token is just the marker — must step back to the phrase token.
        let phrase = vec![0, 240, 480, 720];
        let downbeat = 720 + BAR; // well past the 1-beat threshold
        let ticks: Vec<i32> = phrase
            .into_iter()
            .chain(std::iter::once(downbeat))
            .collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_ne!(result, downbeat);
        // After the marker token is removed, the phrase start is the only usable
        // candidate left.
        assert_eq!(result, 0);
    }

    #[test]
    fn snap_skips_downbeat_marker_when_a_previous_token_exists() {
        // Two phrase tokens, then a single downbeat marker. Should snap to the start
        // of the second phrase, not the marker.
        let token_a = vec![0, 240, 480];
        let token_b_start = 480 + Sequencer::phrase_absolute_gap_ticks();
        let token_b = vec![token_b_start, token_b_start + 240, token_b_start + 480];
        let downbeat = token_b_start + 480 + BAR;
        let ticks: Vec<i32> = token_a
            .into_iter()
            .chain(token_b)
            .chain(std::iter::once(downbeat))
            .collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_eq!(result, token_b_start);
        assert_ne!(result, downbeat);
    }

    #[test]
    fn snap_single_token_window_keeps_first_note() {
        // All notes stay in one token. Keep the token start instead of trimming inward.
        let ticks = vec![0, 240, 480, 720, 960];
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 500, Meter::FOUR_FOUR);
        assert_eq!(result, 0);
    }

    #[test]
    fn snap_no_events_returns_region_start() {
        let clip = Clip::new();
        let result = Sequencer::snap_region_start_to_note_on(&clip, 1234, Meter::FOUR_FOUR);
        assert_eq!(result, 1234);
    }

    #[test]
    fn snap_scored_boundary_detects_boundary_below_one_beat() {
        // Continuous noodling at ~200-tick IOIs (well below 1 beat) with a single
        // larger sub-beat gap into a half-bar boundary. The gap alone is not
        // decisive, but silence + metric placement + span shape is.
        let phrase_a: Vec<i32> = (0..6).map(|i| i * 200).collect();
        let phrase_b_start = Meter::FOUR_FOUR.bar_ticks() / 2;
        let phrase_b: Vec<i32> = (0..6).map(|i| phrase_b_start + i * 200).collect();
        let ticks: Vec<i32> = phrase_a.into_iter().chain(phrase_b).collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_eq!(result, phrase_b_start);
    }

    #[test]
    fn snap_scored_boundary_skips_trailing_downbeat_marker() {
        // Noodling phrase with sub-1-beat boundary, then a trailing downbeat marker
        // separated by another scored gap. The marker is a valid boundary candidate,
        // but snap should step back from that single-note marker to the phrase.
        let phrase_a: Vec<i32> = (0..5).map(|i| i * 200).collect();
        let phrase_b_start = Meter::FOUR_FOUR.bar_ticks() / 2;
        let phrase_b: Vec<i32> = (0..5).map(|i| phrase_b_start + i * 200).collect();
        let downbeat = Meter::FOUR_FOUR.bar_ticks();
        let ticks: Vec<i32> = phrase_a
            .into_iter()
            .chain(phrase_b)
            .chain(std::iter::once(downbeat))
            .collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_eq!(result, phrase_b_start);
        assert_ne!(result, downbeat);
    }

    #[test]
    fn snap_scored_boundary_prefers_metrical_gap_among_subtle_boundaries() {
        // Three noodled gestures. The first larger gap is unmetered and should stay
        // inside the phrase; the later scored bar boundary should start the token.
        let phrase_a: Vec<i32> = (0..5).map(|i| i * 200).collect();
        let phrase_b_start = phrase_a.last().unwrap() + 700;
        let phrase_b: Vec<i32> = (0..5).map(|i| phrase_b_start + i * 200).collect();
        let phrase_c_start = Meter::FOUR_FOUR.bar_ticks();
        let phrase_c: Vec<i32> = (0..5).map(|i| phrase_c_start + i * 200).collect();
        let ticks: Vec<i32> = phrase_a
            .into_iter()
            .chain(phrase_b)
            .chain(phrase_c)
            .collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 0, Meter::FOUR_FOUR);
        assert_eq!(result, phrase_c_start);
    }

    #[test]
    fn snap_scored_boundary_ignores_uniform_playing() {
        // No outlier gap and no phrase-shape cue — keep the first token start.
        let ticks: Vec<i32> = (0..10).map(|i| i * 240).collect();
        let clip = clip_with_note_ons(&ticks);
        let result = Sequencer::snap_region_start_to_note_on(&clip, 1000, Meter::FOUR_FOUR);
        assert_eq!(result, 0);
    }

    #[test]
    fn phrase_token_starts_returns_first_note_and_each_gap_crossing() {
        // Three tokens separated by ≥ 1.5-beat gaps. Should return the start of each.
        let token_a_start = 0;
        let token_b_start = 480 + Sequencer::phrase_absolute_gap_ticks();
        let token_c_start = token_b_start + 240 + Sequencer::phrase_absolute_gap_ticks();
        let ticks: Vec<i32> = vec![token_a_start, 240, 480]
            .into_iter()
            .chain(vec![token_b_start, token_b_start + 240])
            .chain(vec![
                token_c_start,
                token_c_start + 240,
                token_c_start + 480,
            ])
            .collect();
        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);
        assert_eq!(starts, vec![token_a_start, token_b_start, token_c_start]);
    }

    #[test]
    fn phrase_token_starts_empty_for_no_events() {
        let clip = Clip::new();
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);
        assert!(starts.is_empty());
    }

    #[test]
    fn phrase_token_starts_single_phrase_returns_only_first_note() {
        // No qualifying gaps — returns just the first note.
        let ticks = vec![100, 240, 480, 720];
        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);
        assert_eq!(starts, vec![100]);
    }

    #[test]
    fn phrase_token_starts_scores_noodled_boundary() {
        // Sub-1-beat phrase boundary into a half-bar position should be detected
        // when silence, metric strength, and span shape agree.
        let phrase_a: Vec<i32> = (0..5).map(|i| i * 200).collect();
        let phrase_b_start = Meter::FOUR_FOUR.bar_ticks() / 2;
        let phrase_b: Vec<i32> = (0..5).map(|i| phrase_b_start + i * 200).collect();
        let ticks: Vec<i32> = phrase_a.into_iter().chain(phrase_b).collect();
        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);
        assert_eq!(starts, vec![0, phrase_b_start]);
    }

    #[test]
    fn phrase_token_starts_marks_each_scored_phrase_boundary() {
        let phrase_a: Vec<i32> = (0..6).map(|i| i * 200).collect();
        let phrase_b_start = Meter::FOUR_FOUR.bar_ticks() / 2;
        let phrase_b: Vec<i32> = (0..5).map(|i| phrase_b_start + i * 200).collect();
        let phrase_c_start = Meter::FOUR_FOUR.bar_ticks();
        let phrase_c: Vec<i32> = (0..5).map(|i| phrase_c_start + i * 200).collect();
        let ticks: Vec<i32> = phrase_a
            .into_iter()
            .chain(phrase_b)
            .chain(phrase_c)
            .collect();

        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![0, phrase_b_start, phrase_c_start]);
    }

    #[test]
    fn phrase_token_starts_ignores_unmetered_modest_gap() {
        let phrase_a: Vec<i32> = (0..5).map(|i| i * 200).collect();
        let modest_gap_start = phrase_a.last().unwrap() + 700;
        let phrase_b: Vec<i32> = (0..5).map(|i| modest_gap_start + i * 200).collect();
        let prominent_gap_start = Meter::FOUR_FOUR.bar_ticks();
        let phrase_c: Vec<i32> = (0..5).map(|i| prominent_gap_start + i * 200).collect();
        let ticks: Vec<i32> = phrase_a
            .into_iter()
            .chain(phrase_b)
            .chain(phrase_c)
            .collect();

        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![0, prominent_gap_start]);
    }

    #[test]
    fn phrase_token_starts_backtracks_first_visible_token_when_window_cuts_into_it() {
        let token_a_start = 1_000;
        let token_a = vec![token_a_start, token_a_start + 240, token_a_start + 480];
        let token_b_start = 15_000;
        let token_b = vec![token_b_start, token_b_start + 240, token_b_start + 1_600];
        let ticks: Vec<i32> = token_a.into_iter().chain(token_b).collect();
        let clip = clip_with_note_ons(&ticks);

        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);
        let window_ticks = Meter::FOUR_FOUR.bars_to_ticks(PHRASE_DETECTION_WINDOW_BARS);
        let last_tick = *ticks.iter().max().unwrap();

        assert!(token_a_start < last_tick - window_ticks);
        assert_eq!(starts, vec![token_a_start, token_b_start]);
    }

    #[test]
    fn phrase_token_starts_scores_long_last_token_boundary() {
        let token_a = vec![0, 240, 480];
        let token_b_start = 2_000;
        let token_b = vec![token_b_start, token_b_start + 200, token_b_start + 400];
        let refined_last_start = Meter::FOUR_FOUR.bar_ticks();
        let mut token_c: Vec<i32> = vec![refined_last_start, refined_last_start + 200];
        token_c.extend((0..35).map(|i| refined_last_start + 400 + i * 200));
        let ticks: Vec<i32> = token_a.into_iter().chain(token_b).chain(token_c).collect();

        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![0, token_b_start, refined_last_start]);
    }

    #[test]
    fn phrase_token_starts_does_not_refine_short_last_token() {
        let token_a = vec![0, 240, 480];
        let token_b_start = 2_000;
        let token_b = vec![token_b_start, token_b_start + 200, token_b_start + 400];
        let short_gap_split_candidate = 3_000;
        let token_c = vec![
            short_gap_split_candidate,
            short_gap_split_candidate + 200,
            short_gap_split_candidate + 400,
        ];
        let ticks: Vec<i32> = token_a.into_iter().chain(token_b).chain(token_c).collect();

        let clip = clip_with_note_ons(&ticks);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![0, token_b_start]);
    }

    #[test]
    fn phrase_token_starts_uses_note_pair_silence_not_note_on_gap() {
        let pairs = vec![
            (0, 1_600),
            (2_000, 2_200),
            (2_240, 2_440),
            (4_200, 4_400),
            (4_440, 4_640),
        ];

        let clip = clip_with_note_pairs(&pairs);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![0, 4_200]);
    }

    #[test]
    fn phrase_token_starts_leaves_greatest_gap_passive() {
        let pairs = vec![
            (100, 180),
            (660, 740),
            (1_220, 1_300),
            (2_400, 2_480),
            (2_960, 3_040),
            (3_520, 3_600),
            (4_700, 4_780),
            (5_260, 5_340),
            (5_820, 5_900),
        ];

        let clip = clip_with_note_pairs(&pairs);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![100]);
    }

    #[test]
    fn phrase_token_starts_uses_last_note_end_for_windowing() {
        let held_marker_start = BAR * 5;
        let held_marker_end = held_marker_start + time::beats_to_ticks(1.0);
        let window_start =
            held_marker_end - Meter::FOUR_FOUR.bars_to_ticks(PHRASE_DETECTION_WINDOW_BARS);
        let previous_tail_start = 4_140;
        let current_phrase_start = 4_920;
        let pairs = vec![
            (3_900, 4_020),
            (previous_tail_start, 4_860),
            (current_phrase_start, 5_040),
            (5_160, 5_280),
            (5_400, 5_520),
            (held_marker_start, held_marker_end),
        ];

        assert!(previous_tail_start < window_start);
        assert!(current_phrase_start >= window_start);

        let clip = clip_with_note_pairs(&pairs);
        let starts = Sequencer::phrase_token_starts(&clip, Meter::FOUR_FOUR);

        assert_eq!(starts, vec![current_phrase_start, held_marker_start]);
    }

    #[test]
    fn snap_short_late_token_prefers_latest_phrase_token_at_or_after_raw_start() {
        let token_a = vec![0, 240, 480];
        let token_b_start = 6000;
        let token_b = vec![token_b_start, token_b_start + 240, token_b_start + 480];
        let token_c_start = 11000;
        let token_c = vec![token_c_start, token_c_start + 240, token_c_start + 480];
        let short_late_token_start = 14500;
        let short_late_token = vec![short_late_token_start, short_late_token_start + 120];
        let ticks: Vec<i32> = token_a
            .into_iter()
            .chain(token_b)
            .chain(token_c)
            .chain(short_late_token)
            .collect();
        let clip = clip_with_note_ons(&ticks);

        let result = Sequencer::snap_region_start_to_note_on(&clip, 5800, Meter::FOUR_FOUR);

        assert_eq!(result, token_c_start);
    }

    #[test]
    fn snap_short_late_token_prefers_latest_phrase_token_when_raw_start_is_before_it() {
        let token_a = vec![0, 240, 480];
        let token_b_start = 6000;
        let token_b = vec![token_b_start, token_b_start + 240, token_b_start + 480];
        let token_c_start = 11000;
        let token_c = vec![token_c_start, token_c_start + 240, token_c_start + 480];
        let short_late_token_start = 14500;
        let short_late_token = vec![short_late_token_start, short_late_token_start + 120];
        let ticks: Vec<i32> = token_a
            .into_iter()
            .chain(token_b)
            .chain(token_c)
            .chain(short_late_token)
            .collect();
        let clip = clip_with_note_ons(&ticks);

        let result = Sequencer::snap_region_start_to_note_on(&clip, 7000, Meter::FOUR_FOUR);

        assert_eq!(result, token_c_start);
    }

    #[test]
    fn snap_ignores_pre_window_tail_when_last_note_is_held() {
        let held_marker_start = BAR * 5;
        let held_marker_end = held_marker_start + time::beats_to_ticks(1.0);
        let current_phrase_start = 4_920;
        let pairs = vec![
            (3_900, 4_020),
            (4_140, 4_860),
            (current_phrase_start, 5_040),
            (5_160, 5_280),
            (5_400, 5_520),
            (held_marker_start, held_marker_end),
        ];
        let clip = clip_with_note_pairs(&pairs);

        let result = Sequencer::snap_region_start_to_note_on(
            &clip,
            held_marker_start - BAR * 4,
            Meter::FOUR_FOUR,
        );

        assert_eq!(result, current_phrase_start);
    }
}
