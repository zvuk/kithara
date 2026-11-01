use std::num::NonZeroU32;

use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;

use super::{Interval, SourceGrid, SourceGridError};
use crate::{TrackBeat, TrackGrid};

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
