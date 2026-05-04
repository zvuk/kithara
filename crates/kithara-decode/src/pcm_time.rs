use std::time::Duration;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Convert PCM frame count at a given sample rate into a saturating `Duration`.
#[must_use]
pub fn duration_for_frames(sample_rate: u32, frames: u64) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }

    let nanos = u128::from(frames)
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_div(u128::from(sample_rate));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to u64::MAX before cast"
    )]
    {
        Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
    }
}

/// Convert `Duration` back into PCM frame count at a given sample rate.
#[must_use]
pub fn frames_for_duration(sample_rate: u32, duration: Duration) -> usize {
    if sample_rate == 0 {
        return 0;
    }

    let frames = duration
        .as_nanos()
        .saturating_mul(u128::from(sample_rate))
        .saturating_div(NANOS_PER_SECOND);
    frames.min(usize::MAX as u128) as usize
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn duration_for_frames_maps_pcm_frames_to_duration() {
        assert_eq!(duration_for_frames(48_000, 576), Duration::from_millis(12));
    }

    #[kithara::test]
    fn frames_for_duration_maps_duration_to_pcm_frames() {
        assert_eq!(frames_for_duration(48_000, Duration::from_millis(12)), 576);
    }

    #[kithara::test]
    fn zero_sample_rate_is_safe() {
        assert_eq!(duration_for_frames(0, 576), Duration::ZERO);
        assert_eq!(frames_for_duration(0, Duration::from_millis(12)), 0);
    }
}
