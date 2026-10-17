use std::{num::NonZeroU32, ops::RangeInclusive};

use num_traits::ToPrimitive;

use super::source::SourceGridError;
use crate::TrackGrid;

#[derive(Clone, Debug)]
pub(super) struct Interval {
    pub(super) frames: RangeInclusive<f64>,
    pub(super) beats: RangeInclusive<f64>,
}

impl Interval {
    fn new(start: (f64, f64), end: (f64, f64)) -> Result<Self, SourceGridError> {
        if !start.0.is_finite() || !end.0.is_finite() || !start.1.is_finite() || !end.1.is_finite()
        {
            return Err(SourceGridError::Coordinate);
        }
        if start.0 >= end.0 {
            return Err(SourceGridError::NonIncreasingPosition);
        }
        if start.1 >= end.1 {
            return Err(SourceGridError::NonIncreasingBeat);
        }
        Ok(Self {
            frames: start.0..=end.0,
            beats: start.1..=end.1,
        })
    }

    pub(super) fn beat_at(&self, frame: f64) -> Option<f64> {
        interpolate(frame, &self.frames, &self.beats)
    }
    pub(super) fn frame_at(&self, beat: f64) -> Option<f64> {
        interpolate(beat, &self.beats, &self.frames)
    }

    pub(super) fn bpm(&self, rate: NonZeroU32) -> Option<f64> {
        let frames = self.frames.end() - self.frames.start();
        let beats = self.beats.end() - self.beats.start();
        let bpm = beats * f64::from(rate.get()) * 60.0 / frames;
        (bpm.is_finite() && bpm > 0.0).then_some(bpm)
    }
}

fn interpolate(
    value: f64,
    input: &RangeInclusive<f64>,
    output: &RangeInclusive<f64>,
) -> Option<f64> {
    if !input.contains(&value) {
        return None;
    }
    if value == *input.start() {
        return Some(*output.start());
    }
    if value == *input.end() {
        return Some(*output.end());
    }
    let fraction = (value - input.start()) / (input.end() - input.start());
    let result = (output.end() - output.start()).mul_add(fraction, *output.start());
    result.is_finite().then_some(result)
}

fn ordinal(value: i64) -> Option<f64> {
    let scalar = value.to_f64()?;
    (scalar.to_i64() == Some(value)).then_some(scalar)
}

fn prepare_anchors(
    grid: &TrackGrid,
    scale: f64,
    decoded_end: Option<f64>,
) -> Result<Vec<(f64, i64)>, SourceGridError> {
    let mut anchors = grid
        .beats()
        .iter()
        .map(|beat| {
            let frame = beat.at * scale;
            if !frame.is_finite() {
                return Err(SourceGridError::Coordinate);
            }
            if decoded_end.is_some_and(|end| frame > end) {
                return Err(SourceGridError::OutsideExtent {
                    ordinal: beat.ordinal,
                });
            }
            Ok((frame, beat.ordinal))
        })
        .collect::<Result<Vec<_>, SourceGridError>>()?;
    if anchors.is_empty() {
        anchors.push((0.0, 0));
    }
    Ok(anchors)
}

fn extend_intervals(
    intervals: &mut Vec<Interval>,
    grid: &TrackGrid,
    scale: f64,
    decoded_end: Option<f64>,
    beat_frames: f64,
    last: (f64, i64),
) -> Result<(), SourceGridError> {
    let stated_end = grid.duration().map(|seconds| seconds * scale);
    let horizon = match (decoded_end, stated_end) {
        (Some(decoded), Some(stated)) => Some(decoded.min(stated)),
        (bound, None) | (None, bound) => bound,
    };
    if let Some(horizon) = horizon.filter(|end| *end > last.0) {
        let beats = ((horizon - last.0) / beat_frames)
            .floor()
            .to_i64()
            .ok_or(SourceGridError::Tempo)?
            .max(0);
        if beats != 0 {
            let end_ordinal = last
                .1
                .checked_add(beats)
                .ok_or(SourceGridError::Ordinal { ordinal: last.1 })?;
            let end_frame =
                beat_frames.mul_add(beats.to_f64().ok_or(SourceGridError::Tempo)?, last.0);
            if !end_frame.is_finite() {
                return Err(SourceGridError::Coordinate);
            }
            let start =
                ordinal(last.1).ok_or(SourceGridError::InexactStartOrdinal { ordinal: last.1 })?;
            let end = ordinal(end_ordinal).ok_or(SourceGridError::InexactEndOrdinal {
                ordinal: end_ordinal,
            })?;
            intervals.push(Interval::new((last.0, start), (end_frame, end))?);
        }
    }
    Ok(())
}

fn validate_intervals(
    intervals: &[Interval],
    rate: NonZeroU32,
    decoded_end: Option<f64>,
) -> Result<(), SourceGridError> {
    for (index, interval) in intervals.iter().enumerate() {
        if decoded_end.is_some_and(|end| *interval.frames.end() > end) {
            return Err(SourceGridError::IntervalOutsideExtent { index });
        }
        if interval.bpm(rate).is_none() {
            return Err(SourceGridError::IntervalTempo { index });
        }
    }
    Ok(())
}

pub(super) fn prepare(
    grid: &TrackGrid,
    rate: NonZeroU32,
    decoded_frames: Option<u64>,
) -> Result<Vec<Interval>, SourceGridError> {
    let scale = f64::from(rate.get());
    let beat_frames = scale * 60.0 / grid.bpm();
    let decoded_end = decoded_frames.and_then(|frames| frames.to_f64());
    let anchors = prepare_anchors(grid, scale, decoded_end)?;
    if !beat_frames.is_finite() || beat_frames <= 0.0 {
        return Err(SourceGridError::Tempo);
    }
    let first = anchors[0];
    let last = anchors[anchors.len() - 1];
    let mut intervals: Vec<Interval> = Vec::new();
    if first.0 != 0.0 {
        let initial = first
            .1
            .to_f64()
            .ok_or(SourceGridError::Ordinal { ordinal: first.1 })?
            - first.0 / beat_frames;
        if !initial.is_finite() {
            return Err(SourceGridError::Coordinate);
        }
        let end =
            ordinal(first.1).ok_or(SourceGridError::InexactEndOrdinal { ordinal: first.1 })?;
        intervals.push(Interval::new((0.0, initial), (first.0, end))?);
    }
    for pair in anchors.windows(2) {
        let start = ordinal(pair[0].1)
            .ok_or(SourceGridError::InexactStartOrdinal { ordinal: pair[0].1 })?;
        let end =
            ordinal(pair[1].1).ok_or(SourceGridError::InexactEndOrdinal { ordinal: pair[1].1 })?;
        intervals.push(Interval::new((pair[0].0, start), (pair[1].0, end))?);
    }
    extend_intervals(&mut intervals, grid, scale, decoded_end, beat_frames, last)?;
    validate_intervals(&intervals, rate, decoded_end)?;
    Ok(intervals)
}
