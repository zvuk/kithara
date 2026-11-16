use kithara_platform::time::Duration;
use kithara_signal::{FrameCount, SessionFrame};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{
    fixtures::{grid, host, position, source_grid},
    future_law::{advance, rate_at},
};
use crate::{
    Coverage, MIN_SPEED, Slew, SlewCause, SpeedCurve, TrackBeat, TrackGrid, consts::MAX_SPEED,
    phase_error, slew,
};

const HORIZON: u64 = 4 * 48_000;
const EPSILON: f32 = 0.001;

#[kithara::test]
fn a_large_phase_error_makes_bounded_progress_on_a_partial_grid() {
    let host = host(120.0, 4);
    let model = grid(960_000, 960_000, 24_000, 6_000, None);
    let source = source_grid(&model);
    let cue = position(48_000);
    let at = SessionFrame::new(48_000);
    let initial = phase_error(&host, &source, cue, at)
        .expect("covered phase")
        .wrapped(1.0);
    assert!((initial.abs() - 0.25).abs() < 1e-12);
    let Some(Slew::Corrects {
        curve,
        frames,
        coverage,
    }) = slew(
        &host,
        &source,
        cue,
        at,
        EPSILON,
        SlewCause::Drift { from: 1.0 },
        HORIZON,
    )
    else {
        panic!("a partial grid must support a bounded correction");
    };
    assert_eq!(frames, HORIZON);
    assert_eq!(coverage, Coverage::Until(HORIZON));
    let consumed = advance(&curve, 0, HORIZON) / 48_000.0;
    assert!(consumed > 0.0);
    let after = cue + Duration::from_secs_f64(consumed);
    let error = phase_error(
        &host,
        &source,
        after,
        at + FrameCount::new(usize::try_from(HORIZON).expect("bounded horizon")),
    )
    .expect("covered corrected phase")
    .wrapped(1.0);
    assert!(error.abs() < initial.abs() && error.abs() > 0.0);
    for frame in [0, HORIZON / 2, HORIZON - 1, HORIZON] {
        assert!((rate_at(&curve, frame) - 1.0).abs() <= f64::from(EPSILON));
    }
    assert_eq!(rate_at(&curve, HORIZON), 1.0);
}

#[kithara::test]
fn an_unfinished_tempo_ramp_renews_without_snapping_to_its_target() {
    let host = host(144.0, 4);
    let model = grid(48_000_000, 48_000_000, 24_000, 0, None);
    let Some(Slew::Corrects {
        curve,
        frames,
        coverage,
    }) = slew(
        &host,
        &source_grid(&model),
        position(48_000),
        SessionFrame::new(48_000),
        EPSILON,
        SlewCause::Drift { from: 1.0 },
        HORIZON,
    )
    else {
        panic!("the current-speed prefix must be playable");
    };
    assert_eq!(frames, HORIZON);
    assert_eq!(coverage, Coverage::Until(HORIZON));
    let mut previous = 1.0;
    for second in 0..=4 {
        let current = rate_at(&curve, second * 48_000);
        assert!(current > previous && current - previous <= f64::from(EPSILON));
        previous = current;
    }
    assert!(previous < 1.01);
}

fn assert_bounded_rates(curve: &SpeedCurve, from: f64, reference: f64, end: u64) -> f64 {
    let mut probes = vec![0, end];
    if let SpeedCurve::ApproachSteps(laws) = curve {
        for (frame, _) in laws.iter() {
            probes.extend(
                [frame.saturating_sub(1), *frame, frame.saturating_add(1)]
                    .into_iter()
                    .filter(|frame| *frame <= end),
            );
        }
    }
    probes.sort_unstable();
    probes.dedup();
    let mut previous = from;
    for frame in probes {
        let current = rate_at(curve, frame);
        assert!((f64::from(MIN_SPEED)..=f64::from(MAX_SPEED)).contains(&current));
        assert!((current - reference).abs() <= f64::from(EPSILON) + 1e-12);
        assert!((current - previous).abs() <= f64::from(EPSILON) + 1e-12);
        previous = current;
    }
    previous
}

#[kithara::test]
#[case::behind(42_000)]
#[case::ahead(54_000)]
fn repeated_renewals_close_fractional_phase_without_restarting_playback(#[case] cue_frames: u64) {
    const RENEWAL: u64 = HORIZON / 2;

    let host = host(120.0, 4);
    let model = grid(48_000_000, 48_000_000, 24_000, 0, None);
    let source = source_grid(&model);
    let mut cue = position(cue_frames);
    let mut at = SessionFrame::new(48_000);
    let mut from = 1.0;
    let mut error = phase_error(&host, &source, cue, at)
        .expect("covered initial phase")
        .wrapped(1.0)
        .abs();
    assert!((error - 0.25).abs() < 1e-12);
    let mut renewals = 0;
    for _ in 0..96 {
        let Some(Slew::Corrects { curve, .. }) = slew(
            &host,
            &source,
            cue,
            at,
            EPSILON,
            SlewCause::Drift { from },
            HORIZON,
        ) else {
            panic!("observed geometry must cover every renewed window");
        };
        from = assert_bounded_rates(&curve, from, 1.0, RENEWAL);
        let consumed = advance(&curve, 0, RENEWAL) / 48_000.0;
        assert!(consumed > 0.0);
        let before = cue;
        cue += Duration::from_secs_f64(consumed);
        assert!(cue > before, "a renewal only consumes forward media");
        at = at + FrameCount::new(usize::try_from(RENEWAL).expect("renewal window"));
        let next = phase_error(&host, &source, cue, at)
            .expect("covered renewed phase")
            .wrapped(1.0)
            .abs();
        assert!(
            next <= error + 1e-8,
            "renewal increases phase error: {error} -> {next}"
        );
        renewals += 1;
        if renewals == 1 {
            assert!(
                next > 0.2,
                "the first bounded window cannot erase a quarter beat"
            );
        }
        error = next;
        if error < 1e-6 {
            break;
        }
    }
    assert!(renewals > 1);
    assert!(error < 1e-6, "renewed correction did not converge: {error}");
}

#[kithara::test]
#[case::maximum(120.0, 96_000, 72_000, MAX_SPEED, false)]
#[case::minimum(f64::from(MIN_SPEED) * 60.0, 48_000, 12_000, MIN_SPEED, true)]
fn a_speed_boundary_corrects_phase_in_the_available_direction(
    #[case] host_bpm: f64,
    #[case] beat_frames: u64,
    #[case] cue_frames: u64,
    #[case] reference: f32,
    #[case] faster: bool,
) {
    let host = host(host_bpm, 4);
    let model = grid(48_000_000, 48_000_000, beat_frames, 0, None);
    let source = source_grid(&model);
    let cue = position(cue_frames);
    let at = SessionFrame::new(0);
    let initial = phase_error(&host, &source, cue, at)
        .expect("covered phase at a speed boundary")
        .wrapped(1.0);
    assert_eq!(initial, if faster { 0.25 } else { -0.25 });
    let Some(Slew::Corrects { curve, .. }) = slew(
        &host,
        &source,
        cue,
        at,
        EPSILON,
        SlewCause::Drift {
            from: f64::from(reference),
        },
        HORIZON,
    ) else {
        panic!("the other direction around the beat remains playable");
    };
    assert_bounded_rates(&curve, f64::from(reference), f64::from(reference), HORIZON);
    let consumed = advance(&curve, 0, HORIZON);
    let reference_advance = f64::from(reference) * HORIZON.to_f64().expect("window frame count");
    assert!(consumed > 0.0);
    assert_eq!(consumed > reference_advance, faster);
    assert_ne!(
        consumed, reference_advance,
        "the boundary must still permit phase correction"
    );
}

#[kithara::test]
fn phase_correction_stays_playable_when_a_source_interval_reaches_maximum_speed() {
    let host = host(120.0, 4);
    let beats = (0..12)
        .map(|ordinal| TrackBeat {
            at: if ordinal == 0 {
                0.0
            } else {
                2.0 * f64::from(ordinal) - 1.5
            },
            ordinal: i64::from(ordinal),
        })
        .collect();
    let model = TrackGrid::new(beats, Vec::new(), None, 120.0, Some(20.5))
        .expect("source tempo changes from 120 to 30 BPM");
    let source = source_grid(&model);
    let cue = position(18_000);
    let at = SessionFrame::new(0);
    assert_eq!(
        phase_error(&host, &source, cue, at)
            .expect("covered phase")
            .wrapped(1.0),
        -0.25
    );
    let Some(Slew::Corrects { curve, frames, .. }) = slew(
        &host,
        &source,
        cue,
        at,
        EPSILON,
        SlewCause::Drift { from: 1.0 },
        HORIZON,
    ) else {
        panic!("the controller must change direction at the source speed boundary");
    };
    assert_eq!(frames, HORIZON);
    let SpeedCurve::ApproachSteps(laws) = &curve else {
        panic!("the speed law must cross the nonuniform source boundary: {curve:?}");
    };
    let expected = 2.0 / 48_000.0;
    let tolerance = f64::from(EPSILON) / (2.0 * 48_000.0) + 1e-12;
    let mut reached_maximum_interval = false;
    for (index, (frame, law)) in laws.iter().enumerate() {
        for speed in [law.initial(), law.target()] {
            assert!((f64::from(MIN_SPEED)..=f64::from(MAX_SPEED)).contains(&speed));
            reached_maximum_interval |= speed >= f64::from(MAX_SPEED) - f64::from(EPSILON) - 1e-12;
        }
        let end = laws.get(index + 1).map_or(HORIZON + 1, |(frame, _)| *frame);
        assert!(end > *frame);
        for frame in [*frame, end - 1] {
            let before = 18_000.0 + advance(&curve, 0, frame);
            let after = 18_000.0 + advance(&curve, 0, frame + 1);
            assert!(
                after > before,
                "each output frame consumes forward source audio"
            );
            let before = source.beat_at_frame(before).expect("covered frame start");
            let after = source.beat_at_frame(after).expect("covered frame end");
            assert!(
                (after - before - expected).abs() <= tolerance,
                "frame {frame} advances {} source beats instead of {expected}",
                after - before
            );
        }
    }
    assert!(
        reached_maximum_interval,
        "the correction must cross into the 4x interval"
    );
    assert!(advance(&curve, 0, HORIZON) > 0.0);
}
