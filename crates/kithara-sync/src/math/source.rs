use std::{num::NonZeroU32, ops::RangeInclusive};

use kithara_platform::time::Duration;
use kithara_signal::{ExponentialApproach, SessionFrame};
use num_traits::ToPrimitive;

use super::interval::{self, Interval};
use crate::{MIN_SPEED, Meter, SpeedCurve, TempoTrajectory, TrackGrid};

/// Native preparation cannot preserve the recording's musical geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceGridError {
    #[error("a complete source grid requires a known decoded extent")]
    IncompleteAxis,
    #[error("a native coordinate is not finite")]
    Coordinate,
    #[error("beat {ordinal} lies beyond the decoded asset")]
    OutsideExtent { ordinal: i64 },
    #[error("beat ordinal {ordinal} cannot be extended to the stated extent")]
    Ordinal { ordinal: i64 },
    #[error("the stated tempo has no representable beat length in frames")]
    Tempo,
    #[error("start marker ordinal {ordinal} is not exactly representable")]
    InexactStartOrdinal { ordinal: i64 },
    #[error("end marker ordinal {ordinal} is not exactly representable")]
    InexactEndOrdinal { ordinal: i64 },
    #[error("interval end position must follow its start")]
    NonIncreasingPosition,
    #[error("interval end beat must follow its start")]
    NonIncreasingBeat,
    #[error("interval {index} lies beyond the decoded extent")]
    IntervalOutsideExtent { index: usize },
    #[error("interval {index} has an unrepresentable tempo")]
    IntervalTempo { index: usize },
}

/// Source geometry and the observed span eligible for synchronized admission.
#[derive(Debug)]
pub struct SourceGrid {
    intervals: Vec<Interval>,
    rate: NonZeroU32,
    observed: Option<RangeInclusive<Duration>>,
    meter: Option<Meter>,
    complete: bool,
    decoded_frames: Option<u64>,
}

impl SourceGrid {
    /// Prepares checked geometry on its decoded clock without extending observed admission.
    ///
    /// # Errors
    /// Refuses unrepresentable geometry, beats past decoded EOF, or completeness without EOF.
    pub fn new(
        grid: &TrackGrid,
        rate: NonZeroU32,
        decoded_frames: Option<u64>,
        complete: bool,
    ) -> Result<Self, SourceGridError> {
        if complete && decoded_frames.is_none() {
            return Err(SourceGridError::IncompleteAxis);
        }
        let intervals = interval::prepare(grid, rate, decoded_frames)?;
        let observed = grid
            .beats()
            .first()
            .zip(grid.beats().last())
            .and_then(|(first, last)| {
                Duration::try_from_secs_f64(first.at)
                    .ok()
                    .zip(Duration::try_from_secs_f64(last.at).ok())
                    .map(|(first, last)| first..=last)
            });
        Ok(Self {
            intervals,
            rate,
            observed,
            meter: grid.meter(),
            complete,
            decoded_frames,
        })
    }

    fn frame_index(&self, frame: f64) -> Option<usize> {
        if !frame.is_finite()
            || frame < 0.0
            || self
                .decoded_frames
                .and_then(|frames| frames.to_f64())
                .is_some_and(|end| frame > end)
        {
            return None;
        }
        let index = self
            .intervals
            .partition_point(|interval| *interval.frames.start() <= frame)
            .checked_sub(1)?;
        self.intervals
            .get(index)
            .filter(|interval| interval.frames.contains(&frame))
            .map(|_| index)
    }

    fn by_frame(&self, frame: f64) -> Option<&Interval> {
        self.intervals.get(self.frame_index(frame)?)
    }

    fn beat_index(&self, beat: f64) -> Option<usize> {
        if !beat.is_finite() {
            return None;
        }
        let index = self
            .intervals
            .partition_point(|interval| *interval.beats.start() <= beat)
            .checked_sub(1)?;
        self.intervals
            .get(index)
            .filter(|interval| interval.beats.contains(&beat))
            .map(|_| index)
    }

    fn by_beat(&self, beat: f64) -> Option<&Interval> {
        self.intervals.get(self.beat_index(beat)?)
    }

    delegate::delegate! {
        to self.by_frame(frame)? {
            /// Resolves a native decoded frame through the selected affine interval.
            #[must_use]
            #[call(beat_at)]
            pub fn beat_at_frame(&self, frame: f64) -> Option<f64>;
        }
        to self.by_beat(beat)? {
            /// Resolves a musical beat to its native decoded frame.
            #[must_use]
            #[call(frame_at)]
            pub fn frame_at_beat(&self, beat: f64) -> Option<f64>;
        }
    }

    /// Returns the exact local interval tempo on the native decoded clock.
    #[must_use]
    pub fn bpm_at_frame(&self, frame: f64) -> Option<f64> {
        self.by_frame(frame)?.bpm(self.rate)
    }

    /// Returns native-frame and musical bounds, selecting the following interval at a seam.
    #[must_use]
    pub fn interval_at_frame(
        &self,
        frame: f64,
    ) -> Option<(RangeInclusive<f64>, RangeInclusive<f64>)> {
        let interval = self.by_frame(frame)?;
        Some((interval.frames.clone(), interval.beats.clone()))
    }

    /// Returns the interval bounds selected by the musical coordinate.
    #[must_use]
    pub fn interval_at_beat(
        &self,
        beat: f64,
    ) -> Option<(RangeInclusive<f64>, RangeInclusive<f64>)> {
        let interval = self.by_beat(beat)?;
        Some((interval.frames.clone(), interval.beats.clone()))
    }

    /// Native boundary a missing musical coordinate requires, or asset origin without geometry.
    #[must_use]
    pub fn uncovered_frame_at_beat(&self, beat: f64) -> f64 {
        let upper = self
            .intervals
            .partition_point(|interval| *interval.beats.start() <= beat);
        upper
            .checked_sub(1)
            .and_then(|index| self.intervals.get(index))
            .map_or_else(
                || {
                    self.intervals
                        .first()
                        .map_or(0.0, |interval| *interval.frames.start())
                },
                |previous| *previous.frames.end(),
            )
    }

    pub(super) fn covers(&self, position: Duration) -> bool {
        self.observed
            .as_ref()
            .is_some_and(|span| span.contains(&position))
    }

    pub(super) fn phase_axes(&self, seconds: f64) -> Option<(f64, f64, bool)> {
        self.beat_at(seconds)?;
        self.meter.map_or(Some((0.0, 1.0, false)), |meter| {
            Some((
                meter.origin_beat_ordinal.to_f64()?,
                f64::from(meter.beats_per_bar.get()),
                true,
            ))
        })
    }

    pub(super) fn beat_at(&self, seconds: f64) -> Option<f64> {
        self.beat_at_frame(seconds * f64::from(self.rate.get()))
    }

    pub(super) fn media_at(&self, beat: f64) -> Option<f64> {
        Some(self.frame_at_beat(beat)? / f64::from(self.rate.get()))
    }

    pub(super) fn slope_at(&self, seconds: f64) -> Option<f64> {
        Some(60.0 / self.bpm_at_frame(seconds * f64::from(self.rate.get()))?)
    }

    pub(super) fn maximum_slope(&self) -> Option<f64> {
        let frame = self.observed.as_ref()?.start().as_secs_f64() * f64::from(self.rate.get());
        let mut intervals = self.intervals.get(self.frame_index(frame)?..)?.iter();
        let mut interval = intervals.next()?;
        let mut maximum: f64 = 0.0;
        loop {
            maximum = maximum.max(60.0 / interval.bpm(self.rate)?);
            let Some(next) = intervals
                .next()
                .filter(|next| next.frames.start() == interval.frames.end())
            else {
                return Some(maximum);
            };
            interval = next;
        }
    }

    pub(super) fn nearest_media(&self, beats: [f64; 2], requested: Duration) -> Option<Duration> {
        let mut candidates = [None; 2];
        for (candidate, beat) in candidates.iter_mut().zip(beats) {
            if !beat.is_finite() {
                return None;
            }
            let frame = match self.frame_at_beat(beat) {
                Some(frame) => frame,
                None if self.complete => continue,
                None if self
                    .beat_at_frame(self.uncovered_frame_at_beat(beat))
                    .is_some_and(|start| beat < start) =>
                {
                    continue;
                }
                None => return None,
            };
            *candidate =
                Some(Duration::try_from_secs_f64(frame / f64::from(self.rate.get())).ok()?);
        }
        candidates
            .into_iter()
            .flatten()
            .min_by_key(|candidate| requested.abs_diff(*candidate))
    }

    fn interval_at(&self, beat: f64) -> Option<(f64, Option<f64>)> {
        let mut intervals = self.intervals.get(self.beat_index(beat)?..)?.iter();
        let mut interval = intervals.next()?;
        let slope = 60.0 / interval.bpm(self.rate)?;
        loop {
            let Some(next) = intervals
                .next()
                .filter(|next| next.frames.start() == interval.frames.end())
            else {
                return Some((slope, None));
            };
            if 60.0 / next.bpm(self.rate)? != slope {
                return Some((slope, Some(next.beat_at(*interval.frames.end())?)));
            }
            interval = next;
        }
    }
}

fn session_frame(start: SessionFrame, offset: u64) -> Option<SessionFrame> {
    Some(SessionFrame::new(
        i64::from(start).checked_add(i64::try_from(offset).ok()?)?,
    ))
}

fn host_law(
    host: &TempoTrajectory,
    frame: SessionFrame,
    offset: f64,
) -> Option<ExponentialApproach> {
    let anchor = host.anchor_at(frame);
    let initial = anchor.tempo_at(frame) + offset;
    let target = anchor.target_beats_per_second() + offset;
    ExponentialApproach::new(
        initial,
        target,
        if initial == target {
            0.0
        } else {
            anchor.approach().time_constant() * f64::from(host.sample_rate().get())
        },
    )
}

pub(super) fn host_advance(
    host: &TempoTrajectory,
    from: SessionFrame,
    to: SessionFrame,
) -> Option<f64> {
    if to < from {
        return Some(-host_advance(host, to, from)?);
    }
    let mut start = from;
    let mut beats = 0.0;
    for end in host.changes_after(from).filter(|end| *end < to).chain([to]) {
        let frames = end.frames_since(start)?.to_f64()?;
        beats += host_law(host, start, 0.0)?.advance(frames)? / f64::from(host.sample_rate().get());
        start = end;
    }
    Some(beats)
}

fn crossing(law: ExponentialApproach, beats: f64, rate: f64) -> Option<u64> {
    if !beats.is_finite() || beats <= 0.0 {
        return None;
    }
    let mut high = (beats * rate / law.initial().min(law.target()))
        .ceil()
        .to_u64()?;
    high = high.max(1);
    if law.advance(high.to_f64()?)? < beats * rate {
        high = high.checked_add(1)?;
    }
    let mut low = 0;
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if law.advance(middle.to_f64()?)? >= beats * rate {
            high = middle;
        } else {
            low = middle;
        }
    }
    Some(high)
}

fn push_law(
    laws: &mut Vec<(u64, ExponentialApproach)>,
    frame: u64,
    law: ExponentialApproach,
) -> Option<()> {
    if law.initial() < f64::from(MIN_SPEED)
        || law.target() < f64::from(MIN_SPEED)
        || law.initial() > 4.0
        || law.target() > 4.0
    {
        return None;
    }
    if let Some((at, previous)) = laws.last_mut()
        && *at == frame
    {
        *previous = law;
    } else if laws
        .last()
        .is_some_and(|(_, previous)| *previous == law && law.time_constant() == 0.0)
    {
        return Some(());
    } else {
        laws.push((frame, law));
    }
    Some(())
}

#[derive(Clone, Copy)]
enum CorrectionOffset {
    Media(f64),
    Beat(f64),
}

impl CorrectionOffset {
    fn beats(self, slope: f64) -> f64 {
        match self {
            Self::Media(offset) => offset / slope,
            Self::Beat(offset) => offset,
        }
    }
}

pub(super) struct Correction {
    pub(super) phase: f64,
    pub(super) starts: u64,
    pub(super) scale: f64,
    pub(super) epsilon: f32,
}

fn bridge_beat(
    source: &SourceGrid,
    host: ExponentialApproach,
    mut beat: f64,
    offset: CorrectionOffset,
    rate: f64,
) -> Option<f64> {
    if let CorrectionOffset::Beat(offset) = offset {
        return Some(
            beat + ExponentialApproach::new(
                host.initial() + offset,
                host.target() + offset,
                host.time_constant(),
            )?
            .advance(1.0)?
                / rate,
        );
    }
    let mut elapsed = 0.0;
    while elapsed < 1.0 {
        let (slope, end) = source.interval_at(beat)?;
        let law = ExponentialApproach::new(
            host.rate_at(elapsed)? + offset.beats(slope),
            host.target() + offset.beats(slope),
            host.time_constant(),
        )?;
        let remaining = 1.0 - elapsed;
        let reached = beat + law.advance(remaining)? / rate;
        let Some(end) = end.filter(|end| reached > *end) else {
            return Some(reached);
        };
        let mut low = 0.0;
        let mut high = remaining;
        loop {
            let middle = low + (high - low) / 2.0;
            if middle == low || middle == high {
                break;
            }
            if law.advance(middle)? / rate < end - beat {
                low = middle;
            } else {
                high = middle;
            }
        }
        let through = elapsed + high;
        if through <= elapsed {
            return None;
        }
        elapsed = through;
        beat = end;
    }
    Some(beat)
}

struct CurveSchedule {
    boundaries: Vec<u64>,
    landing: Option<(Duration, u64)>,
}

fn curve_boundaries(
    host: &TempoTrajectory,
    start: SessionFrame,
    landing: Option<(Duration, SessionFrame)>,
    offsets: &[(u64, f64)],
    correction: Option<&Correction>,
) -> Option<CurveSchedule> {
    let mut boundaries = host
        .changes_after(start)
        .map(|frame| frame.frames_since(start))
        .collect::<Option<Vec<_>>>()?;
    boundaries.extend(
        offsets
            .windows(2)
            .filter(|pair| pair[0].1 != pair[1].1)
            .map(|pair| pair[1].0),
    );
    if let Some(correction) = &correction {
        boundaries.push(correction.starts);
    }
    let landing = match landing {
        Some((position, at)) => Some((position, at.frames_since(start)?)),
        None => None,
    };
    if let Some((_, at)) = landing {
        boundaries.push(at);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    Some(CurveSchedule {
        boundaries,
        landing,
    })
}

fn landed_interval(
    source: &SourceGrid,
    mut beat: f64,
    frame: u64,
    landing: Option<(Duration, u64)>,
) -> Option<(f64, f64, Option<f64>)> {
    if let Some((position, at)) = landing
        && frame == at
    {
        beat = source.beat_at(position.as_secs_f64())?;
    }
    let (slope, source_end) = source.interval_at(beat)?;
    Some((beat, slope, source_end))
}

fn correction_terminal(
    host: &TempoTrajectory,
    source: &SourceGrid,
    start: SessionFrame,
    first_beat: f64,
    beat: f64,
    frame: u64,
    correction: &Correction,
) -> Option<(u64, f64)> {
    let rate = f64::from(host.sample_rate().get());
    let phase = correction.phase + beat
        - first_beat
        - host_advance(host, start, session_frame(start, frame)?)?;
    let frames = (phase.abs() * rate * correction.scale / f64::from(correction.epsilon))
        .ceil()
        .to_u64()?;
    let end = frame.checked_add(frames)?;
    let offset = if frames == 0 {
        0.0
    } else {
        -phase * rate / frames.to_f64()?
    };
    let target =
        beat + host_advance(
            host,
            session_frame(start, frame)?,
            session_frame(start, end)?,
        )? + offset * frames.to_f64()? / rate;
    source.media_at(target)?;
    Some((end, offset))
}

fn source_law(
    host: &TempoTrajectory,
    start: SessionFrame,
    frame: u64,
    slope: f64,
    terminal: Option<(u64, f64)>,
    offsets: &[(u64, f64)],
    laws: &mut Vec<(u64, ExponentialApproach)>,
) -> Option<(ExponentialApproach, CorrectionOffset)> {
    let offset = match terminal {
        Some((end, offset)) if frame < end => CorrectionOffset::Beat(offset),
        Some(_) => CorrectionOffset::Beat(0.0),
        None => CorrectionOffset::Media(
            offsets
                .get(
                    offsets
                        .partition_point(|(at, _)| *at <= frame)
                        .checked_sub(1)?,
                )?
                .1,
        ),
    };
    let law = host_law(host, session_frame(start, frame)?, offset.beats(slope))?;
    push_law(laws, frame, law.scaled(slope)?)?;
    Some((law, offset))
}

struct CurveCrossing {
    through: u64,
    source_crossing: Option<u64>,
}

fn next_curve_frame(
    next: Option<u64>,
    source_end: Option<f64>,
    law: ExponentialApproach,
    beat: f64,
    rate: f64,
    frame: u64,
) -> Result<Option<CurveCrossing>, ()> {
    let source_crossing = match source_end {
        Some(end) => Some(crossing(law, end - beat, rate).ok_or(())?),
        None => None,
    };
    let through = match (next, source_crossing) {
        (Some(next), Some(crossing)) => next.min(frame.checked_add(crossing).ok_or(())?),
        (Some(next), None) => next,
        (None, Some(crossing)) => frame.checked_add(crossing).ok_or(())?,
        (None, None) => return Ok(None),
    };
    Ok(Some(CurveCrossing {
        through,
        source_crossing,
    }))
}

fn bridge_source_law(
    source: &SourceGrid,
    host: &TempoTrajectory,
    start: SessionFrame,
    frames: std::ops::Range<u64>,
    beat: f64,
    law: ExponentialApproach,
    offset: CorrectionOffset,
) -> Option<(f64, f64)> {
    let rate = f64::from(host.sample_rate().get());
    let before = frames.end.checked_sub(1)?;
    let preceding = beat + law.advance(before.checked_sub(frames.start)?.to_f64()?)? / rate;
    let reached = bridge_beat(
        source,
        host_law(host, session_frame(start, before)?, 0.0)?,
        preceding,
        offset,
        rate,
    )?;
    let speed = (source.media_at(reached)? - source.media_at(preceding)?) * rate;
    Some((reached, speed))
}

fn curve_reached(
    source: &SourceGrid,
    host: &TempoTrajectory,
    start: SessionFrame,
    frames: std::ops::Range<u64>,
    source_basis: (f64, Option<f64>, Option<u64>),
    law: (ExponentialApproach, CorrectionOffset),
    laws: &mut Vec<(u64, ExponentialApproach)>,
) -> Option<f64> {
    let (beat, source_end, source_crossing) = source_basis;
    let (law, offset) = law;
    let rate = f64::from(host.sample_rate().get());
    let elapsed = frames.end.checked_sub(frames.start)?.to_f64()?;
    let mut reached = beat + law.advance(elapsed)? / rate;
    if source_end.is_some_and(|end| reached > end)
        && source_crossing
            .is_some_and(|crossing| frames.start.checked_add(crossing) == Some(frames.end))
    {
        let before = frames.end.checked_sub(1)?;
        let (bridged, speed) = bridge_source_law(source, host, start, frames, beat, law, offset)?;
        reached = bridged;
        push_law(laws, before, ExponentialApproach::new(speed, speed, 0.0)?)?;
    }
    Some(reached)
}

fn speed_curve(laws: Vec<(u64, ExponentialApproach)>) -> Option<SpeedCurve> {
    if laws.len() > 1 {
        Some(SpeedCurve::ApproachSteps(laws.into()))
    } else {
        let first = laws.first()?.1;
        let rounded = first.target().to_f32()?;
        if first.time_constant() == 0.0 && f64::from(rounded) == first.target() {
            Some(SpeedCurve::Constant(rounded))
        } else {
            Some(SpeedCurve::Approach(first))
        }
    }
}

pub(super) fn curve(
    host: &TempoTrajectory,
    source: &SourceGrid,
    media: Duration,
    start: SessionFrame,
    landing: Option<(Duration, SessionFrame)>,
    offsets: &[(u64, f64)],
    correction: Option<&Correction>,
) -> Option<(SpeedCurve, u64)> {
    let rate = f64::from(host.sample_rate().get());
    let first_beat = source.beat_at(media.as_secs_f64())?;
    let mut beat = first_beat;
    let CurveSchedule {
        mut boundaries,
        landing,
    } = curve_boundaries(host, start, landing, offsets, correction)?;
    let mut frame = 0;
    let mut terminal = None;
    let mut laws = Vec::new();
    loop {
        let (landed, slope, source_end) = landed_interval(source, beat, frame, landing)?;
        beat = landed;
        if let Some(correction) = &correction
            && frame >= correction.starts
            && terminal.is_none()
        {
            let (end, offset) =
                correction_terminal(host, source, start, first_beat, beat, frame, correction)?;
            terminal = Some((end, offset));
            boundaries.push(end);
            boundaries.sort_unstable();
            boundaries.dedup();
        }
        let (law, offset) = source_law(host, start, frame, slope, terminal, offsets, &mut laws)?;
        let next = boundaries.iter().copied().find(|next| *next > frame);
        let Some(CurveCrossing {
            through,
            source_crossing,
        }) = next_curve_frame(next, source_end, law, beat, rate, frame).ok()?
        else {
            break;
        };
        beat = curve_reached(
            source,
            host,
            start,
            frame..through,
            (beat, source_end, source_crossing),
            (law, offset),
            &mut laws,
        )?;
        frame = through;
    }
    let frames = terminal.map_or(0, |(end, _)| end);
    Some((speed_curve(laws)?, frames))
}

#[cfg(test)]
#[path = "tests/source.rs"]
mod tests;
