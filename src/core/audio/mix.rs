//! Pure per-track mixer arithmetic: the dB ⇄ fader-position taper the arranger
//! track header draws and drags against, and the linear L/R gain pair the
//! instrument mixer multiplies each voice by. No other module duplicates this —
//! `core::plugin_host::mixer` and `view::display` both call in here.

/// Fader floor. At or below this a track is silent — gain is exactly `0.0` and
/// the readout shows `-inf`, so the bottom of the bar is a true mute rather
/// than merely very quiet.
pub(crate) const MIN_DB: f32 = -60.0;

/// Fader ceiling — a little makeup headroom above unity, matching a typical
/// DAW channel strip.
pub(crate) const MAX_DB: f32 = 6.0;

/// `(fader position, dB)` breakpoints of the piecewise-linear fader taper,
/// position ascending. Unity (0 dB) sits at 0.78 of the travel — the
/// DAW-conventional spot — so most of the bar is spent in the useful −12..0 dB
/// range and resolution is finest around unity, rather than the ~0.91 a
/// straight dB-linear map would give.
const TAPER: [(f32, f32); 5] = [
    (0.00, MIN_DB),
    (0.25, -30.0),
    (0.50, -12.0),
    (0.78, 0.0),
    (1.00, MAX_DB),
];

/// Maps a normalised fader position (`0.0` = bottom / empty, `1.0` = top /
/// full) to dB along [`TAPER`].
pub(crate) fn db_from_fader_pos(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    for w in TAPER.windows(2) {
        let (p0, d0) = w[0];
        let (p1, d1) = w[1];
        if p <= p1 {
            let t = (p - p0) / (p1 - p0);
            return d0 + t * (d1 - d0);
        }
    }
    MAX_DB
}

/// Inverse of [`db_from_fader_pos`] — where a given dB value sits on the bar,
/// as a `0.0..=1.0` fraction.
pub(crate) fn fader_pos_from_db(db: f32) -> f32 {
    let db = db.clamp(MIN_DB, MAX_DB);
    for w in TAPER.windows(2) {
        let (p0, d0) = w[0];
        let (p1, d1) = w[1];
        if db <= d1 {
            let t = (db - d0) / (d1 - d0);
            return p0 + t * (p1 - p0);
        }
    }
    1.0
}

/// Linear per-channel gain `(left, right)` for a stereo voice at `volume_db`
/// and `pan` (`-1.0` hard left … `0.0` centre … `1.0` hard right).
///
/// This is a **balance** control, not an equal-power pan pot: a hosted
/// instrument already outputs stereo, so centre must pass both channels
/// untouched (`1.0, 1.0`) and hard-one-side fully mutes the other. A sin/cos
/// law would pull centre down 3 dB, which is wrong for an already-stereo
/// source — this matches what Ableton's track pan does on a stereo track.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn gains_for(volume_db: f32, pan: f32) -> (f32, f32) {
    let amp = if volume_db <= MIN_DB {
        0.0
    } else {
        10f32.powf(volume_db / 20.0)
    };
    let pan = pan.clamp(-1.0, 1.0);
    let l = amp * (1.0 - pan).min(1.0);
    let r = amp * (1.0 + pan).min(1.0);
    (l, r)
}

/// Whether one track should be heard, given its own mute / solo flags and
/// whether *any* track in the project is soloed. Mute wins over solo on the
/// same track (matching Ableton): a muted track is silent even if also soloed.
/// With nothing soloed anywhere, every unmuted track plays.
///
/// The sequencer applies this to gate note emission (`Sequencer::tick`); the
/// instrument mixer applies it as a `0.0` output-gain factor so a muted track's
/// reverb / delay tail is cut too, not just its new notes.
pub(crate) fn track_is_audible(muted: bool, soloed: bool, any_solo: bool) -> bool {
    !muted && (!any_solo || soloed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn gains_unity_centre_passes_through() {
        let (l, r) = gains_for(0.0, 0.0);
        assert!(close(l, 1.0, 1e-6), "l={l}");
        assert!(close(r, 1.0, 1e-6), "r={r}");
    }

    #[test]
    fn gains_hard_left_mutes_right_and_vice_versa() {
        let (l, r) = gains_for(0.0, -1.0);
        assert!(close(l, 1.0, 1e-6));
        assert_eq!(r, 0.0);

        let (l, r) = gains_for(0.0, 1.0);
        assert_eq!(l, 0.0);
        assert!(close(r, 1.0, 1e-6));
    }

    #[test]
    fn gains_floor_is_silent() {
        assert_eq!(gains_for(MIN_DB, 0.0), (0.0, 0.0));
        assert_eq!(gains_for(MIN_DB - 10.0, -0.5), (0.0, 0.0));
    }

    #[test]
    fn gains_minus_six_db_is_about_half_amplitude() {
        let (l, r) = gains_for(-6.0, 0.0);
        assert!(close(l, 0.501_187, 1e-4), "l={l}");
        assert!(close(r, 0.501_187, 1e-4));
    }

    #[test]
    fn taper_round_trips_at_breakpoints() {
        for (p, db) in TAPER {
            assert!(close(db_from_fader_pos(p), db, 1e-4), "pos {p}");
            assert!(close(fader_pos_from_db(db), p, 1e-4), "db {db}");
        }
    }

    #[test]
    fn taper_round_trips_across_range() {
        for i in 0..=100 {
            let p = i as f32 / 100.0;
            let back = fader_pos_from_db(db_from_fader_pos(p));
            assert!(close(back, p, 1e-3), "p={p} back={back}");
        }
    }

    #[test]
    fn taper_is_monotonic() {
        let mut prev = f32::NEG_INFINITY;
        for i in 0..=200 {
            let db = db_from_fader_pos(i as f32 / 200.0);
            assert!(db >= prev - 1e-4, "not monotonic at {i}: {db} < {prev}");
            prev = db;
        }
    }

    #[test]
    fn track_is_audible_truth_table() {
        // No solo anywhere: unmuted plays, muted doesn't.
        assert!(track_is_audible(false, false, false));
        assert!(!track_is_audible(true, false, false));
        // A solo exists elsewhere: an unmuted, unsoloed track is silenced.
        assert!(!track_is_audible(false, false, true));
        // Soloed and unmuted: plays.
        assert!(track_is_audible(false, true, true));
        // Muted wins over solo on the same track.
        assert!(!track_is_audible(true, true, true));
    }

    #[test]
    fn taper_clamps_out_of_range_input() {
        assert_eq!(db_from_fader_pos(-1.0), MIN_DB);
        assert_eq!(db_from_fader_pos(2.0), MAX_DB);
        assert_eq!(fader_pos_from_db(-200.0), 0.0);
        assert_eq!(fader_pos_from_db(200.0), 1.0);
    }
}
