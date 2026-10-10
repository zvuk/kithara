use kithara_command::SendError;
use kithara_render::{LaneFrame, LaneProtocol, bridge::SlotMark};
use kithara_signal::SessionFrame;
use kithara_warp::SpeedCurve;
use num_traits::ToPrimitive;

use crate::PlayError;

pub(super) fn lane_refusal(error: &SendError<LaneProtocol>) -> PlayError {
    match error {
        SendError::Full(_) => PlayError::Full("lane"),
        SendError::Target(_) | SendError::Closed(_) => PlayError::Closed,
    }
}

pub(super) fn session_at(mark: SlotMark, lane: LaneFrame) -> Option<SessionFrame> {
    if lane.segment != mark.lane.segment {
        return None;
    }
    let distance = i64::try_from(lane.frame.abs_diff(mark.lane.frame)).ok()?;
    let frame = if lane.frame >= mark.lane.frame {
        i64::from(mark.session).checked_add(distance)
    } else {
        i64::from(mark.session).checked_sub(distance)
    }?;
    Some(SessionFrame::new(frame))
}

pub(super) fn curve_speed(curve: &SpeedCurve, origin: f32, frame: u64) -> Result<f32, PlayError> {
    match curve {
        SpeedCurve::Constant(speed) => Ok(*speed),
        SpeedCurve::Ramp { to, frames } => (f64::from(origin)
            + (f64::from(*to) - f64::from(origin))
                * (frame.min(frames.get()).to_f64().ok_or(PlayError::Untimed)?
                    / frames.get().to_f64().ok_or(PlayError::Untimed)?))
        .to_f32()
        .ok_or(PlayError::Untimed),
        SpeedCurve::Steps(steps) => Ok(steps
            .iter()
            .rev()
            .find(|(at, _)| *at <= frame)
            .map_or(origin, |(_, speed)| *speed)),
        _ => Err(PlayError::Internal(
            "unsupported planned speed curve".into(),
        )),
    }
}

pub(super) fn curve_area(
    curve: &SpeedCurve,
    origin: f32,
    start: u64,
    end: u64,
) -> Result<f64, PlayError> {
    match curve {
        SpeedCurve::Constant(speed) => {
            Ok((end - start).to_f64().ok_or(PlayError::Untimed)? * f64::from(*speed))
        }
        SpeedCurve::Ramp { to, frames } => {
            let duration = frames.get().to_f64().ok_or(PlayError::Untimed)?;
            let area = |frame: u64| -> Result<f64, PlayError> {
                let ramp = frame.min(frames.get()).to_f64().ok_or(PlayError::Untimed)?;
                Ok(f64::from(origin) * ramp
                    + (f64::from(*to) - f64::from(origin)) * ramp * ramp / (2.0 * duration)
                    + frame
                        .saturating_sub(frames.get())
                        .to_f64()
                        .ok_or(PlayError::Untimed)?
                        * f64::from(*to))
            };
            Ok(area(end)? - area(start)?)
        }
        SpeedCurve::Steps(steps) => {
            let mut area = 0.0;
            let mut cursor = start;
            let mut speed = f64::from(origin);
            for &(frame, next) in steps.iter() {
                if frame > end {
                    break;
                }
                if frame > cursor {
                    area += (frame - cursor).to_f64().ok_or(PlayError::Untimed)? * speed;
                    cursor = frame;
                }
                speed = f64::from(next);
            }
            Ok(area + (end - cursor).to_f64().ok_or(PlayError::Untimed)? * speed)
        }
        _ => Err(PlayError::Internal(
            "unsupported planned speed curve".into(),
        )),
    }
}

pub(super) fn curve_end(
    curve: &SpeedCurve,
    origin: f32,
    start: u64,
    mut remaining: f64,
    limit: Option<u64>,
) -> Result<Option<u64>, PlayError> {
    if remaining <= 0.0 {
        return Ok(Some(start));
    }
    let mut cursor = start;
    let end = limit.unwrap_or(u64::MAX);
    while cursor < end {
        let speed = f64::from(curve_speed(curve, origin, cursor)?);
        if !speed.is_finite() || speed <= 0.0 {
            return Err(PlayError::Internal(
                "planned speed must be finite and positive".into(),
            ));
        }
        let (boundary, slope) = match curve {
            SpeedCurve::Ramp { to, frames } if cursor < frames.get() => (
                end.min(frames.get()),
                (f64::from(*to) - f64::from(origin))
                    / frames.get().to_f64().ok_or(PlayError::Untimed)?,
            ),
            SpeedCurve::Constant(_) | SpeedCurve::Ramp { .. } => (end, 0.0),
            SpeedCurve::Steps(steps) => (
                steps
                    .iter()
                    .find(|(frame, _)| *frame > cursor)
                    .map_or(end, |(frame, _)| end.min(*frame)),
                0.0,
            ),
            _ => {
                return Err(PlayError::Internal(
                    "unsupported planned speed curve".into(),
                ));
            }
        };
        let area = curve_area(curve, origin, cursor, boundary)?;
        if remaining <= area {
            let distance = if slope == 0.0 {
                remaining / speed
            } else {
                2.0 * remaining
                    / (speed + (speed * speed + 2.0 * slope * remaining).max(0.0).sqrt())
            };
            let rounded = distance.ceil().to_u64().ok_or(PlayError::Untimed)?;
            return cursor
                .checked_add(rounded)
                .map(|frame| Some(frame.min(boundary)))
                .ok_or(PlayError::Untimed);
        }
        remaining -= area;
        cursor = boundary;
    }
    Ok(None)
}
