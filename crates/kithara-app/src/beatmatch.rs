use kithara::audio::{BeatGrid, SourceFrame, TrackBeatMap, analysis::TrackAnalysis};
use num_traits::ToPrimitive;

struct Consts;

impl Consts {
    /// Seconds in a minute, converting between BPM and beat length.
    const SECONDS_PER_MINUTE: f64 = 60.0;
}

/// The speed that puts a deck's beats on the session's, folded by whole
/// beat multiples.
///
/// A 70 BPM record on a 140 BPM session is not stretched to double speed — it
/// plays as it is and counts half-time, which is what every DJ program does
/// and what keeps the stretch near unity where it sounds best. Folding is part
/// of the answer, not a guard on it: it is what makes the whole 20..=299 range
/// reachable at all.
#[must_use]
pub fn speed(session_bpm: f64, deck_bpm: f64) -> Option<f64> {
    let raw = session_bpm / deck_bpm;
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    // `floor(log2 + 1/2)` rather than `round`, so both ends of the octave fold
    // the same way and the range stays half-open.
    let folded = raw / (raw.log2() + 0.5).floor().exp2();
    folded.is_finite().then_some(folded)
}

/// Beats the deck runs ahead of the session, in `(-0.5, 0.5]`.
///
/// Measured against the nearer beat either way: a deck a hair behind the beat
/// is nudged forward by that hair, not dragged back a whole beat.
#[must_use]
pub fn ahead(track_beat: f64, session_beat: f64) -> Option<f64> {
    let offset = (track_beat - session_beat).rem_euclid(1.0);
    if !offset.is_finite() {
        return None;
    }
    Some(if offset > 0.5 { offset - 1.0 } else { offset })
}

/// Where the deck's playhead sits on its own analysed beat grid.
///
/// `position` is the deck's place in the track in source seconds — the axis
/// its duration is on, and the axis the analysed markers are on.
#[must_use]
pub fn track_beat_at(analysis: &TrackAnalysis, position: f64) -> Option<f64> {
    let rate = analysis.source_sample_rate()?;
    // Building the map on the source rate leaves the markers where the
    // analysis put them, so the playhead needs no conversion to meet them.
    let map = TrackBeatMap::new(analysis, rate).ok()?;
    let frame = SourceFrame::new(position * f64::from(rate.get())).ok()?;
    map.track_beat_at(frame).map(f64::from)
}

/// The deck's tempo, read off the beats the analysis marked.
///
/// Deliberately not [`BeatGrid::bpm`]. That scalar is a second, independent
/// account of the same thing, and on real records it disagrees with the
/// markers — measured at 571 BPM against markers marching at 149, and at 169
/// against 131. The markers are what the beat map, the phase move and the
/// bound schedule all read, so the tempo is read off them too; otherwise a
/// deck is matched to a tempo none of its own beats are on.
#[must_use]
pub fn deck_bpm(analysis: &TrackAnalysis) -> Option<f64> {
    let rate = analysis.source_sample_rate()?;
    let beats = analysis.beat().map(BeatGrid::beats)?;
    let period = beat_period(beats)?;
    let bpm = Consts::SECONDS_PER_MINUTE * f64::from(rate.get()) / period;
    (bpm.is_finite() && bpm > 0.0).then_some(bpm)
}

/// How many source frames one beat lasts, over the whole marked span.
///
/// The mean over the span, not the middle of the gap distribution. A detector
/// places its marks on a coarse frame — measured gaps snapped between 460 and
/// 480 ms on a record whose beat was near 472 — so any single gap, and the
/// median of them, is off by up to a twentieth of a beat. Spread over hundreds
/// of beats that placement noise cancels; a median only picks whichever side
/// of the frame is more crowded.
///
/// This works because the marks arrive already cleaned of double detections.
/// Guarding against them a second time here would put the same rule in two
/// places and let them disagree.
fn beat_period(beats: &[u64]) -> Option<f64> {
    let span = beats.last()?.checked_sub(*beats.first()?)?.to_f64()?;
    let intervals = beats.len().checked_sub(1)?.to_f64()?;
    (intervals >= 1.0).then(|| span / intervals)
}

/// Source seconds the playhead moves to give back `ahead` beats.
///
/// The beat is the *track's* own, so the move is measured at the track's
/// analysed tempo rather than the session's: it is the same audio either way,
/// and stretching it is the speed's job.
#[must_use]
pub fn seek_target(position: f64, ahead: f64, deck_bpm: f64) -> Option<f64> {
    let beat_seconds = Consts::SECONDS_PER_MINUTE / deck_bpm;
    let moved = ahead.mul_add(-beat_seconds, position);
    // A deck early in its first beat has nothing behind it to give the time
    // back from, so it waits out the rest of the beat instead.
    let target = if moved < 0.0 {
        moved + beat_seconds
    } else {
        moved
    };
    (target.is_finite() && target >= 0.0).then_some(target)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use ::kithara::audio::{BeatGrid, analysis::TrackAnalysis};
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{ahead, deck_bpm, seek_target, speed, track_beat_at};

    struct Fixture;

    impl Fixture {
        /// Source rate the fixture's markers are expressed in.
        const RATE: u32 = 48_000;
        /// Beats the fixture lays down.
        const BEATS: u64 = 64;
        /// The half-open octave a folded speed must live in, `[1/√2, √2)`.
        const FOLD_LOW: f64 = std::f64::consts::FRAC_1_SQRT_2;
        const FOLD_HIGH: f64 = std::f64::consts::SQRT_2;
    }

    /// An even grid whose markers march at `marched` while its own scalar
    /// claims `reported` — the shape every real record turned out to have.
    fn analysis_claiming(reported: f64, marched: f64) -> TrackAnalysis {
        let beat_frames = (f64::from(Fixture::RATE) * 60.0 / marched)
            .round()
            .to_u64()
            .expect("invariant: a fixture beat fits a frame count");
        let beats: Vec<u64> = (0..Fixture::BEATS).map(|i| i * beat_frames).collect();
        TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(reported, beats, Vec::new(), Vec::new())),
            None,
            Fixture::BEATS * beat_frames,
            NonZeroU32::new(Fixture::RATE).expect("invariant: the fixture rate is non-zero"),
        )
    }

    /// An even grid at `bpm`, markers starting at frame zero.
    fn analysis(bpm: f64) -> TrackAnalysis {
        let beat_frames = (f64::from(Fixture::RATE) * 60.0 / bpm)
            .round()
            .to_u64()
            .expect("invariant: a fixture beat fits a frame count");
        let beats: Vec<u64> = (0..Fixture::BEATS).map(|i| i * beat_frames).collect();
        let downbeats: Vec<u64> = beats.iter().step_by(4).copied().collect();
        TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(bpm, beats, downbeats, Vec::new())),
            None,
            Fixture::BEATS * beat_frames,
            NonZeroU32::new(Fixture::RATE).expect("invariant: the fixture rate is non-zero"),
        )
    }

    /// Measured on real records: the scalar said 571 BPM where the markers
    /// marched at 149, and 169 where they marched at 131. Everything that
    /// places a beat reads the markers, so the tempo must come from there too.
    #[kithara::test]
    fn the_tempo_comes_from_the_markers_not_from_the_grid_s_own_scalar() {
        let bpm =
            deck_bpm(&analysis_claiming(571.429, 149.0)).expect("a marked record has a tempo");
        assert!((bpm - 149.0).abs() < 0.1, "got {bpm}");
    }

    /// A detector that places every marker on a coarse frame makes each gap
    /// wrong by up to half that frame. Averaged over the run it must come out
    /// on the beat anyway — that is the whole point of taking the span.
    #[kithara::test]
    fn marker_placement_noise_averages_out_over_the_run() {
        let period = f64::from(Fixture::RATE) * 60.0 / 128.0;
        let snap = f64::from(Fixture::RATE) / 100.0;
        let beats: Vec<u64> = (0..Fixture::BEATS)
            .filter_map(|i| {
                let exact = i.to_f64()? * period;
                ((exact / snap).round() * snap).to_u64()
            })
            .collect();
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(999.0, beats, Vec::new(), Vec::new())),
            None,
            Fixture::BEATS * 44_100,
            NonZeroU32::new(Fixture::RATE).expect("invariant: the fixture rate is non-zero"),
        );
        let bpm = deck_bpm(&analysis).expect("a marked record has a tempo");
        assert!((bpm - 128.0).abs() < 0.2, "got {bpm}");
    }

    #[kithara::test]
    fn a_record_with_a_single_marker_names_no_tempo() {
        let analysis = TrackAnalysis::with_source_rate(
            Some(BeatGrid::new(120.0, vec![0], Vec::new(), Vec::new())),
            None,
            Fixture::RATE.into(),
            NonZeroU32::new(Fixture::RATE).expect("invariant: the fixture rate is non-zero"),
        );
        assert_eq!(deck_bpm(&analysis), None);
    }

    #[kithara::test]
    fn a_record_with_no_grid_at_all_names_no_tempo() {
        let analysis = TrackAnalysis::with_source_rate(
            None,
            None,
            Fixture::RATE.into(),
            NonZeroU32::new(Fixture::RATE).expect("invariant: the fixture rate is non-zero"),
        );
        assert_eq!(deck_bpm(&analysis), None);
    }

    #[kithara::test]
    fn a_deck_already_at_the_session_tempo_is_left_at_its_own_speed() {
        assert_eq!(speed(128.0, 128.0), Some(1.0));
    }

    #[kithara::test]
    fn a_half_time_record_is_locked_by_the_beat_multiple_not_by_stretching() {
        let folded = speed(140.0, 70.0).expect("a positive tempo pair resolves");
        assert!(
            (folded - 1.0).abs() < 1e-12,
            "a 70 BPM record on a 140 BPM session must play as it is, got {folded}"
        );
    }

    #[kithara::test]
    fn a_double_time_record_folds_the_other_way() {
        let folded = speed(70.0, 140.0).expect("a positive tempo pair resolves");
        assert!(
            (folded - 1.0).abs() < 1e-12,
            "a 140 BPM record on a 70 BPM session must play as it is, got {folded}"
        );
    }

    /// The whole 20..=299 range a DJ expects, against every session tempo in
    /// it: the folded speed never leaves the octave around unity, so the
    /// stretch engine is never asked for a ratio it cannot hold.
    #[kithara::test]
    fn the_folded_speed_never_leaves_the_octave_around_unity() {
        for session in 20..=299 {
            for deck in 20..=299 {
                let folded = speed(f64::from(session), f64::from(deck))
                    .expect("every tempo pair in the DJ range resolves");
                assert!(
                    (Fixture::FOLD_LOW..Fixture::FOLD_HIGH).contains(&folded),
                    "a {session} BPM session against a {deck} BPM record asks for {folded}"
                );
            }
        }
    }

    #[kithara::test]
    fn a_tempo_of_zero_names_no_speed() {
        assert_eq!(speed(128.0, 0.0), None);
    }

    #[kithara::test]
    fn a_deck_a_quarter_beat_early_is_a_quarter_beat_ahead() {
        let error = ahead(8.25, 8.0).expect("finite beats resolve");
        assert!((error - 0.25).abs() < 1e-12, "got {error}");
    }

    #[kithara::test]
    fn a_deck_just_behind_the_beat_is_nudged_forward_not_dragged_back() {
        let error = ahead(7.9, 8.0).expect("finite beats resolve");
        assert!((error + 0.1).abs() < 1e-12, "got {error}");
    }

    #[kithara::test]
    fn the_phase_error_never_exceeds_half_a_beat() {
        for step in 0..1_000 {
            let track = f64::from(step) / 250.0;
            let error = ahead(track, 3.0).expect("finite beats resolve");
            assert!(error.abs() <= 0.5, "beat {track} reported {error} beats");
        }
    }

    #[kithara::test]
    fn a_whole_bar_of_offset_is_no_offset_at_all() {
        let error = ahead(12.0, 8.0).expect("finite beats resolve");
        assert!(error.abs() < 1e-12, "got {error}");
    }

    #[kithara::test]
    fn the_playhead_reads_the_beat_the_analysis_put_under_it() {
        // 120 BPM is half a second a beat, so four seconds in is beat eight.
        let beat = track_beat_at(&analysis(120.0), 4.0).expect("a marked position resolves");
        assert!((beat - 8.0).abs() < 1e-6, "got {beat}");
    }

    #[kithara::test]
    fn a_playhead_between_markers_reads_a_fractional_beat() {
        let beat = track_beat_at(&analysis(120.0), 4.25).expect("a marked position resolves");
        assert!((beat - 8.5).abs() < 1e-6, "got {beat}");
    }

    #[kithara::test]
    fn a_playhead_past_the_analysed_markers_reads_no_beat() {
        assert_eq!(track_beat_at(&analysis(120.0), 10_000.0), None);
    }

    #[kithara::test]
    fn a_deck_ahead_of_the_beat_is_sent_back_by_that_much() {
        // A quarter beat at 120 BPM is an eighth of a second.
        let target = seek_target(10.0, 0.25, 120.0).expect("a positive tempo resolves");
        assert!((target - 9.875).abs() < 1e-12, "got {target}");
    }

    #[kithara::test]
    fn a_deck_behind_the_beat_is_sent_forward() {
        let target = seek_target(10.0, -0.25, 120.0).expect("a positive tempo resolves");
        assert!((target - 10.125).abs() < 1e-12, "got {target}");
    }

    #[kithara::test]
    fn a_deck_too_close_to_the_start_waits_out_the_beat_instead_of_going_negative() {
        let target = seek_target(0.1, 0.4, 120.0).expect("a positive tempo resolves");
        assert!(
            target >= 0.0,
            "the playhead was sent before the track: {target}"
        );
    }
}
