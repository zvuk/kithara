use kithara_platform::time::Duration;
use kithara_signal::SessionFrame;
use num_traits::ToPrimitive;

use super::{
    plan::{CorrectionPlan, CorrectionStep},
    source::{self, Correction, SourceGrid},
};
use crate::{Bound, MIN_SPEED, SessionBeat, SpeedCurve, TempoTrajectory, TrackGrid};

/// Source beats ahead of the Host phase, modulo the stated beat period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseError {
    pub beats: f64,
    pub period: f64,
}

impl PhaseError {
    pub(super) fn shortest(self) -> f64 {
        let phase = self.beats.rem_euclid(1.0);
        if phase > 0.5 { phase - 1.0 } else { phase }
    }
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

/// The Host law composed with the actual source anchor and an optional jump landing.
#[must_use]
pub fn speed_curve(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
    landing: Option<(Duration, SessionFrame)>,
) -> Option<SpeedCurve> {
    source::curve(host, grid, position, at, landing, &[(0, 0.0)], None).map(|(curve, _)| curve)
}

/// Replaces the remaining correction and closes source beat phase without a jump.
#[must_use]
pub fn corrected_speed_curve(
    host: &TempoTrajectory,
    grid: &SourceGrid,
    position: Duration,
    at: SessionFrame,
    from: f64,
    epsilon: f32,
    commanded: bool,
) -> Option<(SpeedCurve, u64)> {
    let local = grid.slope_at(position.as_secs_f64())?;
    let reference = host.anchor_at(at).tempo_at(at) * local;
    let plan = checked_correction(if commanded { reference } else { from }, reference, epsilon)?;
    let offsets = plan.media_offsets(host.sample_rate(), reference)?;
    let starts = offsets.last()?.0;
    let error = phase_error(host, grid, position, at)?;
    let correction = Correction {
        phase: error.shortest(),
        starts: if commanded { starts.max(1) } else { starts },
        scale: grid.maximum_slope()?,
        epsilon,
    };
    source::curve(host, grid, position, at, None, &offsets, Some(&correction))
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
    if !from.is_finite()
        || from < f64::from(MIN_SPEED)
        || !to.is_finite()
        || to < f64::from(MIN_SPEED)
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
