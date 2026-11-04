use std::num::NonZeroU32;

use kithara_platform::time::Duration;
use kithara_signal::{ExponentialApproach, SessionFrame};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{Correction, Coverage, Interval, SourceGrid, SourceGridError, Span, curve};
use crate::{SpeedCurve, TrackBeat, TrackGrid};

fn reference_by_frame(grid: &SourceGrid, frame: f64) -> Option<&Interval> {
    if !frame.is_finite()
        || frame < 0.0
        || grid
            .decoded_frames
            .and_then(|frames| frames.to_f64())
            .is_some_and(|end| frame > end)
    {
        return None;
    }
    grid.intervals
        .partition_point(|interval| *interval.frames.start() <= frame)
        .checked_sub(1)
        .and_then(|index| grid.intervals.get(index))
        .filter(|interval| interval.frames.contains(&frame))
}

fn reference_by_beat(grid: &SourceGrid, beat: f64) -> Option<&Interval> {
    if !beat.is_finite() {
        return None;
    }
    grid.intervals
        .partition_point(|interval| *interval.beats.start() <= beat)
        .checked_sub(1)
        .and_then(|index| grid.intervals.get(index))
        .filter(|interval| interval.beats.contains(&beat))
}

fn reference_maximum(grid: &SourceGrid) -> Option<f64> {
    let mut frame = grid.observed.as_ref()?.start().as_secs_f64() * f64::from(grid.rate.get());
    let mut maximum: f64 = 0.0;
    loop {
        maximum = maximum.max(60.0 / reference_by_frame(grid, frame)?.bpm(grid.rate)?);
        let interval = reference_by_frame(grid, frame)?;
        let end = *interval.frames.end();
        let Some(next) = reference_by_frame(grid, end) else {
            return Some(maximum);
        };
        if *next.frames.end() == end {
            return Some(maximum);
        }
        frame = end;
    }
}

fn reference_interval(grid: &SourceGrid, beat: f64) -> Option<(f64, Option<f64>)> {
    let mut interval = reference_by_beat(grid, beat)?;
    let slope = 60.0 / interval.bpm(grid.rate)?;
    loop {
        let end = *interval.frames.end();
        let next = reference_by_frame(grid, end)?;
        if *next.frames.end() == end {
            return Some((slope, None));
        }
        if 60.0 / next.bpm(grid.rate)? != slope {
            return Some((slope, Some(reference_by_frame(grid, end)?.beat_at(end)?)));
        }
        interval = next;
    }
}

fn bounds(interval: &Interval) -> [u64; 4] {
    [
        interval.frames.start().to_bits(),
        interval.frames.end().to_bits(),
        interval.beats.start().to_bits(),
        interval.beats.end().to_bits(),
    ]
}

fn compare_reference(grid: &SourceGrid) {
    assert_eq!(
        grid.maximum_slope().map(f64::to_bits),
        reference_maximum(grid).map(f64::to_bits)
    );
    let boundaries = grid.intervals.iter().flat_map(|interval| {
        [
            *interval.frames.start(),
            *interval.frames.end(),
            (interval.frames.start() + interval.frames.end()) / 2.0,
            *interval.beats.start(),
            *interval.beats.end(),
            (interval.beats.start() + interval.beats.end()) / 2.0,
        ]
    });
    for value in boundaries.chain([-0.0, 0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY]) {
        for query in [value.next_down(), value, value.next_up()] {
            assert_eq!(
                grid.interval_at(query)
                    .map(|(slope, end)| (slope.to_bits(), end.map(f64::to_bits))),
                reference_interval(grid, query)
                    .map(|(slope, end)| (slope.to_bits(), end.map(f64::to_bits))),
                "beat {query:?}: {grid:?}"
            );
            assert_eq!(
                grid.interval_at_frame(query).map(|(frames, beats)| [
                    frames.start().to_bits(),
                    frames.end().to_bits(),
                    beats.start().to_bits(),
                    beats.end().to_bits(),
                ]),
                reference_by_frame(grid, query).map(bounds),
                "frame {query:?}: {grid:?}"
            );
            assert_eq!(
                grid.interval_at_beat(query).map(|(frames, beats)| [
                    frames.start().to_bits(),
                    frames.end().to_bits(),
                    beats.start().to_bits(),
                    beats.end().to_bits(),
                ]),
                reference_by_beat(grid, query).map(bounds),
                "public beat {query:?}: {grid:?}"
            );
        }
    }
}

#[kithara::test]
fn source_traversals_preserve_every_affine_seam_and_incomplete_axis() {
    let markers: &[&[(f64, i64)]] = &[
        &[],
        &[(0.0, 0)],
        &[(1.0, -2)],
        &[(0.0, 0), (0.5, 1), (1.0, 2), (1.75, 3), (2.5, 4), (3.0, 5)],
        &[
            (0.125, -4),
            (0.5, -2),
            (1.375, -1),
            (2.0, 3),
            (3.5, 6),
            (7.0, 14),
        ],
        &[
            (0.0, 0),
            (1.0, 1),
            (2.0_f64.next_up(), 2),
            (3.0_f64.next_up(), 3),
        ],
    ];
    for points in markers {
        for rate in [
            NonZeroU32::MIN,
            NonZeroU32::new(44_100).expect("rate"),
            NonZeroU32::new(48_000).expect("rate"),
        ] {
            for bpm in [30.0, 120.0, 132.0] {
                for duration in [None, Some(8.0)] {
                    let model = TrackGrid::new(
                        points
                            .iter()
                            .map(|&(at, ordinal)| TrackBeat { at, ordinal })
                            .collect(),
                        Vec::new(),
                        None,
                        bpm,
                        duration,
                    )
                    .expect("valid sparse source markers");
                    for decoded_frames in [None, Some(8 * u64::from(rate.get()))] {
                        for complete in [false, true] {
                            let source = SourceGrid::new(&model, rate, decoded_frames, complete);
                            if complete && decoded_frames.is_none() {
                                assert_eq!(source.err(), Some(SourceGridError::IncompleteAxis));
                            } else {
                                compare_reference(&source.expect("representable source geometry"));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[kithara::test]
fn an_uncovered_gap_ends_the_connected_source_law() {
    let source = SourceGrid {
        intervals: vec![
            Interval {
                frames: 0.0..=1.0,
                beats: 0.0..=1.0,
            },
            Interval {
                frames: 2.0..=3.0,
                beats: 2.0..=3.0,
            },
        ],
        rate: NonZeroU32::MIN,
        observed: Some(Duration::ZERO..=Duration::from_secs(3)),
        meter: None,
        complete: false,
        decoded_frames: None,
    };
    compare_reference(&source);
    assert_eq!(source.maximum_slope(), Some(1.0));
    assert_eq!(source.interval_at(0.5), Some((1.0, None)));
    assert_eq!(source.interval_at(1.5), None);
}

const RATE: u32 = 48_000;

fn steady_host() -> crate::TempoTrajectory {
    crate::TempoTrajectory::new(
        crate::TempoStep {
            frame: SessionFrame::new(0),
            beat: crate::SessionBeat::default(),
            tempo: crate::Tempo::new(120.0).expect("tempo"),
        },
        std::num::NonZeroU16::new(4).expect("meter"),
        NonZeroU32::new(RATE).expect("rate"),
    )
}

/// A grid whose beat spacing alternates by a few frames, as analysis jitter
/// does, so no two neighbouring intervals share a slope.
fn jittered(beats: i64) -> SourceGrid {
    let mut at = 0_u64;
    let beats = (0..beats)
        .map(|ordinal| {
            let beat = TrackBeat {
                at: at.to_f64().expect("frame") / f64::from(RATE),
                ordinal,
            };
            at += if ordinal % 2 == 0 { 24_000 } else { 24_048 };
            beat
        })
        .collect();
    let model = TrackGrid::new(beats, Vec::new(), None, 120.0, None).expect("jittered grid");
    SourceGrid::new(&model, NonZeroU32::new(RATE).expect("rate"), None, false)
        .expect("source geometry")
}

fn laws(curve: &SpeedCurve) -> Vec<(u64, ExponentialApproach)> {
    match curve {
        SpeedCurve::ApproachSteps(laws) => laws.to_vec(),
        other => panic!("expected stepped laws, received {other:?}"),
    }
}

fn plan(
    source: &SourceGrid,
    correction: Option<&Correction>,
    horizon: u64,
) -> (Vec<(u64, ExponentialApproach)>, u64, Coverage) {
    let (curve, frames, coverage) = curve(
        &steady_host(),
        source,
        Duration::ZERO,
        Span {
            start: SessionFrame::new(0),
            landing: None,
            horizon,
        },
        &[(0, 0.0)],
        correction,
    )
    .expect("planned source curve");
    (laws(&curve), frames, coverage)
}

/// The bounded plan is the unbounded plan cut at its coverage, and that cut
/// is the first source boundary at or past `reach`.
fn assert_cut(
    bounded: &[(u64, ExponentialApproach)],
    coverage: Coverage,
    unbounded: &[(u64, ExponentialApproach)],
    reach: u64,
) {
    let Coverage::Until(until) = coverage else {
        panic!("a bounded plan inside the grid covers a frame span, got {coverage:?}");
    };
    assert!(until >= reach, "coverage {until} stops before {reach}");
    let prefix: Vec<_> = unbounded
        .iter()
        .copied()
        .take_while(|(at, _)| *at < until)
        .collect();
    assert_eq!(bounded, prefix.as_slice());
    assert!(
        unbounded.iter().any(|(at, _)| *at == until),
        "coverage {until} is a law boundary of the unbounded plan"
    );
    assert!(
        bounded
            .iter()
            .filter(|(at, _)| *at >= reach)
            .all(|(at, _)| *at + 1 == until),
        "only the bridge law into the cut may start past the reach"
    );
}

#[kithara::test]
fn a_long_grid_is_planned_only_to_the_first_boundary_past_the_horizon() {
    let source = jittered(400);
    let (unbounded, _, end) = plan(&source, None, u64::MAX);
    assert_eq!(end, Coverage::GridEnd);
    assert!(unbounded.len() > 300, "one law per jittered interval");

    let horizon = 4 * u64::from(RATE);
    let (bounded, frames, coverage) = plan(&source, None, horizon);
    assert_eq!(frames, 0);
    assert_cut(&bounded, coverage, &unbounded, horizon);
    assert!(
        bounded.len() < 20,
        "{} laws for a 4 s horizon",
        bounded.len()
    );
}

#[kithara::test]
fn a_correction_longer_than_the_horizon_is_planned_whole_and_a_horizon_past_it() {
    let source = jittered(100);
    let correction = Correction {
        phase: 0.02,
        starts: 0,
        scale: source.maximum_slope().expect("slope"),
        epsilon: 0.001,
    };
    let (unbounded, whole, _) = plan(&source, Some(&correction), u64::MAX);
    let horizon = u64::from(RATE);
    assert!(whole > 2 * horizon, "the correction outlasts the horizon");

    let (bounded, frames, coverage) = plan(&source, Some(&correction), horizon);
    assert_eq!(frames, whole);
    assert_cut(&bounded, coverage, &unbounded, whole + horizon);
}

#[kithara::test]
fn a_grid_ending_inside_the_horizon_is_followed_to_its_end() {
    let source = jittered(6);
    let (unbounded, _, _) = plan(&source, None, u64::MAX);
    let (bounded, _, coverage) = plan(&source, None, 60 * u64::from(RATE));
    assert_eq!(coverage, Coverage::GridEnd);
    assert_eq!(bounded, unbounded);
}

/// Output frames advanced by stepped laws over `start..end`.
fn advance(laws: &[(u64, ExponentialApproach)], start: u64, end: u64) -> f64 {
    laws.iter()
        .enumerate()
        .filter_map(|(index, (at, law))| {
            let after = laws.get(index + 1).map_or(end, |(next, _)| *next);
            let from = start.max(*at);
            let to = end.min(after);
            (from < to).then(|| {
                law.advance_over(
                    (from - at).to_f64().expect("offset"),
                    (to - from).to_f64().expect("span"),
                )
                .expect("valid law")
            })
        })
        .sum()
}

fn rate_at(laws: &[(u64, ExponentialApproach)], frame: u64) -> f64 {
    let (at, law) = laws
        .iter()
        .rev()
        .find(|(at, _)| *at <= frame)
        .expect("a law covers the frame");
    law.rate_at((frame - at).to_f64().expect("offset"))
        .expect("valid law")
}

#[kithara::test]
fn a_renewal_from_the_planned_position_continues_the_unbounded_plan() {
    let source = jittered(400);
    let horizon = 4 * u64::from(RATE);
    let (unbounded, _, _) = plan(&source, None, u64::MAX);
    let (first, _, coverage) = plan(&source, None, horizon);
    let Coverage::Until(seam) = coverage else {
        panic!("the first plan stops inside the grid");
    };
    let planned = Duration::from_secs_f64(advance(&first, 0, seam) / f64::from(RATE));
    let (second, _, _) = curve(
        &steady_host(),
        &source,
        planned,
        Span {
            start: SessionFrame::new(i64::try_from(seam).expect("seam")),
            landing: None,
            horizon,
        },
        &[(0, 0.0)],
        None,
    )
    .expect("renewed plan");
    let second = laws(&second);
    for offset in [0, 1, 4_799, 48_000, 96_001, horizon - 1] {
        let renewed = rate_at(&second, offset);
        let whole = rate_at(&unbounded, seam + offset);
        assert!(
            (renewed - whole).abs() < 1e-9,
            "rate {offset} past the seam: {renewed} vs {whole}"
        );
    }
    let renewed = advance(&second, 0, horizon);
    let whole = advance(&unbounded, seam, seam + horizon);
    assert!(
        (renewed - whole).abs() < 1e-3,
        "media frames past the seam: {renewed} vs {whole}"
    );
}
