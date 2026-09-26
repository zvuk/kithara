use std::num::NonZeroU32;

use kithara_platform::time::Duration;

use crate::{SignalError, consts};

pub(super) fn duration_for(sample_rate: NonZeroU32, frames: u64) -> Result<Duration, SignalError> {
    let nanos = u128::from(frames)
        .checked_mul(consts::NANOS_PER_SECOND)
        .and_then(|value| value.checked_div(u128::from(sample_rate.get())))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| SignalError::DurationOverflow {
            frames,
            sample_rate: sample_rate.get(),
        })?;
    Ok(Duration::from_nanos(nanos))
}

pub(super) fn frames_for(
    sample_rate: NonZeroU32,
    duration: Duration,
) -> Result<usize, SignalError> {
    let duration_nanos = duration.as_nanos();
    let frames = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate.get()))
        .and_then(|value| value.checked_div(consts::NANOS_PER_SECOND))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| SignalError::FrameCountOverflow {
            duration_nanos,
            sample_rate: sample_rate.get(),
        })?;
    Ok(frames)
}

pub(super) fn frame_at(sample_rate: NonZeroU32, timestamp: Duration) -> Result<u64, SignalError> {
    let rate = u64::from(sample_rate.get());
    let subsec_frames = u64::from(timestamp.subsec_nanos())
        .checked_mul(rate)
        .and_then(|value| value.checked_add(500_000_000))
        .and_then(|value| value.checked_div(1_000_000_000));
    timestamp
        .as_secs()
        .checked_mul(rate)
        .and_then(|frames| frames.checked_add(subsec_frames?))
        .ok_or_else(|| SignalError::FrameCountOverflow {
            duration_nanos: timestamp.as_nanos(),
            sample_rate: sample_rate.get(),
        })
}
