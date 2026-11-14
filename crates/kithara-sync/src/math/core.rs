use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;
use num_traits::ToPrimitive;

use super::{
    plan::{CorrectionPlan, CorrectionStep},
    source::{self, Correction, Coverage, CurveError, SourceGrid, Span},
};
use crate::{
    Bound, MIN_SPEED, SessionBeat, SpeedCurve, TempoTrajectory, TrackGrid, consts::MAX_SPEED,
};

/// Source beats ahead of the Host phase, modulo the stated beat period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseError {
    pub beats: f64,
    pub period: f64,
}

impl PhaseError {
    /// The error wrapped to the nearest multiple of `period` beats.
    pub(super) fn wrapped(self, period: f64) -> f64 {
        let phase = self.beats.rem_euclid(period);
        if phase > period / 2.0 {
            phase - period
        } else {
            phase
        }
    }
}

/// What asks a sounding deck to slew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SlewCause {
    /// A tempo command: the slew starts at the new reference speed and closes
    /// the phase at any size.
    Commanded,
    /// Phase drift: the slew ramps from the deck's current speed.
    Drift {
        /// Speed the deck plays at when the slew starts.
        from: f64,
    },
}

/// How a sounding deck closes its source beat phase.
#[derive(Clone, Debug, PartialEq)]
pub enum Slew {
    /// Bounded speed steps correct phase within the planned window.
    Corrects {
        /// Speed law from the correction start.
        curve: SpeedCurve,
        /// Frames from the correction start to the end of this correction window.
        frames: u64,
        /// How far the law follows the source grid.
        coverage: Coverage,
    },
    /// The planned window needs source geometry that is not available yet.
    Uncovered,
}

/// Whether a position lies inside the grid's inclusive observed beat span.
#[must_use]
pub fn covers(grid: &TrackGrid, position: Duration) -> bool {
    match (grid.beats().first(), grid.beats().last()) {
        (Some(first), Some(last)) => {
            match (
                Duration::try_from_secs_f64(first.at),
                Duration::try_from_secs_f64(last.at),
            ) {
                (Ok(first), Ok(last)) => (first..=last).contains(&position),
                _ => false,
            }
        }
        _ => false,
    }
}

/// The cue's phase on the source grid, relative to its declared meter.
fn phase_at(grid: &SourceGrid, position: Duration) -> Option<f64> {
    if !grid.covers(position) {
        return None;
    }
    let beat = grid.beat_at(position.as_secs_f64())?;
    let (origin, stride, _) = grid.phase_axes(position.as_secs_f64())?;
    Some((beat - origin).rem_euclid(stride))
}

/// Track speed from the reached Host tempo and the source's local beat interval.
#[must_use]
pub fn speed(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
) -> Option<f32> {
    (host.anchor_at(at).tempo_at(at) * grid.slope_at(position.as_secs_f64())?).to_f32()
}

/// The Host law composed with the actual source anchor and an optional jump
/// landing, planned `horizon` output frames past the landing.
#[must_use]
pub fn speed_curve(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
    landing: Option<(Duration, SessionFrame)>,
    horizon: u64,
) -> Option<(SpeedCurve, Coverage)> {
    let span = Span {
        start: at,
        landing,
        horizon,
    };
    source::curve(host, grid, position, span, &[(0, 0.0)], None)
        .ok()
        .map(|(curve, _, coverage)| (curve, coverage))
}

/// What a slew starts from at its frame.
struct SlewStart {
    /// Speed that holds the Host tempo at the position.
    reference: f64,
    /// Speed steps from the slew's start speed to the reference.
    plan: CorrectionPlan,
    /// Source phase ahead of the Host at the position.
    error: PhaseError,
}

/// Plans bounded speed correction from `at`, renewing long corrections after `horizon` frames;
/// `None` when the grid has no speed law there.
#[must_use]
pub fn slew(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
    epsilon: f32,
    cause: SlewCause,
    horizon: u64,
) -> Option<Slew> {
    let start = slew_start(host, grid, position, at, epsilon, cause)?;
    let span = Span {
        start: at,
        landing: None,
        horizon,
    };
    close_slew(host, grid, position, span, epsilon, cause, &start)
}

/// The speed law correcting phase from `start` within its source geometry.
fn close_slew(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    span: Span,
    epsilon: f32,
    cause: SlewCause,
    start: &SlewStart,
) -> Option<Slew> {
    let offsets = start
        .plan
        .media_offsets(host.sample_rate(), start.reference)?;
    let starts = offsets.last()?.0;
    let correction = Correction {
        phase: start.error.wrapped(1.0),
        starts: match cause {
            SlewCause::Commanded => starts.max(1),
            SlewCause::Drift { .. } => starts,
        },
        scale: grid.maximum_slope()?,
        epsilon,
    };
    match source::curve(host, grid, position, span, &offsets, Some(&correction)) {
        Ok((curve, frames, coverage)) => Some(Slew::Corrects {
            curve,
            frames,
            coverage,
        }),
        Err(CurveError::PastGrid) => Some(Slew::Uncovered),
        Err(CurveError::NoLaw) => None,
    }
}

fn slew_start(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
    epsilon: f32,
    cause: SlewCause,
) -> Option<SlewStart> {
    let local = grid.slope_at(position.as_secs_f64())?;
    let reference = host.anchor_at(at).tempo_at(at) * local;
    let from = match cause {
        SlewCause::Commanded => reference,
        SlewCause::Drift { from, .. } => from,
    };
    Some(SlewStart {
        reference,
        plan: checked_correction(from, reference, epsilon)?,
        error: phase_error(host, grid, position, at)?,
    })
}

/// The nearest in-phase entry on the requested side of a frame.
///
/// On constant tempo this is a Host boundary plus the media offset divided
/// by speed; beat inversion also accounts for intervening Host tempo steps.
/// Candidates are compared on rounded session frames, inclusively on both sides.
#[must_use]
pub fn entry(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    bound: Bound,
) -> Option<SessionFrame> {
    let offset = phase_at(grid, position)?;
    let stride = if grid.phase_axes(position.as_secs_f64())?.2 {
        host.beats_per_bar()
    } else {
        1.0
    };
    let (frame, after) = match bound {
        Bound::AtOrAfter(frame) => (frame, true),
        Bound::AtOrBefore(frame) => (frame, false),
    };
    let direction = if after { 1.0 } else { -1.0 };
    let admitted = |candidate| {
        if after {
            candidate >= frame
        } else {
            candidate <= frame
        }
    };
    let frame_at = |ordinal| {
        let beat = SessionBeat::new(ordinal * stride + offset).ok()?;
        host.try_frame_at(beat)
    };
    let mut ordinal = ((f64::from(host.beat_at(frame)) - offset) / stride).floor();
    let mut candidate = frame_at(ordinal)?;
    while !admitted(candidate) {
        let next = ordinal + direction;
        if next == ordinal {
            return None;
        }
        ordinal = next;
        candidate = frame_at(ordinal)?;
    }
    loop {
        let next = ordinal - direction;
        if next == ordinal {
            break;
        }
        let Some(closer) = frame_at(next) else {
            break;
        };
        if !admitted(closer) {
            break;
        }
        ordinal = next;
        candidate = closer;
    }
    Some(candidate)
}

/// Source beat phase ahead of the Host, modulo a bar or one beat without meter.
#[must_use]
pub fn phase_error(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
) -> Option<PhaseError> {
    let phase = phase_at(grid, position)?;
    let stride = if grid.phase_axes(position.as_secs_f64())?.2 {
        host.beats_per_bar()
    } else {
        1.0
    };
    let host_phase = f64::from(host.beat_at(at)).rem_euclid(stride);
    Some(PhaseError {
        beats: (phase - host_phase).rem_euclid(stride),
        period: stride,
    })
}

/// The nearer mapped source position with the requested future Host phase.
#[must_use]
pub fn jump_target(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
) -> Option<Duration> {
    let beat = grid.beat_at(position.as_secs_f64())?;
    let (origin, stride, metered) = grid.phase_axes(position.as_secs_f64())?;
    let host_stride = if metered { host.beats_per_bar() } else { 1.0 };
    let phase = f64::from(host.beat_at(at)).rem_euclid(host_stride);
    if phase >= stride {
        return None;
    }
    let phase = phase + origin;
    let earlier = phase + ((beat - phase) / stride).floor() * stride;
    grid.nearest_media([earlier, earlier + stride], position)
}

/// Returns bounded correction steps, or `None` for invalid or unrepresentable inputs.
#[must_use]
pub fn checked_correction(from: f64, to: f64, epsilon: f32) -> Option<CorrectionPlan> {
    let playable = f64::from(MIN_SPEED)..=f64::from(MAX_SPEED);
    if !playable.contains(&from)
        || !playable.contains(&to)
        || !epsilon.is_finite()
        || epsilon <= 0.0
    {
        return None;
    }
    let mut steps: Vec<CorrectionStep> = Vec::new();
    let mut current = from;
    while current != to {
        current = step_towards(current, to, epsilon)?;
        let seconds = if current == to { 0.0 } else { 1.0 };
        steps.push(CorrectionStep {
            speed: current,
            seconds,
        });
    }
    steps.push(CorrectionStep {
        speed: to,
        seconds: 0.0,
    });
    Some(CorrectionPlan { steps })
}

fn step_towards(from: f64, to: f64, epsilon: f32) -> Option<f64> {
    let epsilon = f64::from(epsilon);
    let shift = (to - from).clamp(-epsilon, epsilon);
    let mut next = from + shift;
    if (next - from).abs() > epsilon {
        next = if shift > 0.0 {
            next.next_down()
        } else {
            next.next_up()
        };
    }
    (next.is_finite()
        && next >= f64::from(MIN_SPEED)
        && next != from
        && (next - from).abs() <= epsilon)
        .then_some(next)
}
