use std::num::{NonZeroU32, NonZeroUsize};

use kithara_platform::time::Duration;

use crate::{FrameCount, SampleCount, SignalError, time};

/// Core decoded-audio format information.
///
/// `Default` is intentionally absent because a zero sample rate is invalid.
#[derive(Clone, Copy, Debug, derive_more::Display, PartialEq, Eq)]
#[display("{sample_rate} Hz, {channels} channels")]
pub struct AudioSpec {
    pub sample_rate: NonZeroU32,
    pub channels: u16,
}

impl AudioSpec {
    #[must_use]
    pub const fn new(channels: u16, sample_rate: NonZeroU32) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    /// Checked non-zero channel count for layout operations.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError::ChannelCountZero`] when `channels` is zero.
    pub fn channel_count(self) -> Result<NonZeroUsize, SignalError> {
        NonZeroUsize::new(usize::from(self.channels)).ok_or(SignalError::ChannelCountZero)
    }

    /// Convert an absolute frame coordinate to duration without saturation.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the duration is not representable.
    pub fn duration_for(self, frames: u64) -> Result<Duration, SignalError> {
        time::duration_for(self.sample_rate, frames)
    }

    /// Convert a timestamp to its nearest absolute frame, rounded half-up.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the absolute frame is not representable.
    pub fn frame_at(self, timestamp: Duration) -> Result<u64, SignalError> {
        time::frame_at(self.sample_rate, timestamp)
    }

    /// Elapsed time on this spec's frame grid, saturating at zero.
    /// Endpoints within nanosecond precision of whole frames use their frame difference.
    /// Fractional or unrepresentable endpoints retain their time difference.
    #[must_use]
    pub fn elapsed_since(self, origin: Duration, time: Duration) -> Duration {
        let frame_on_grid = |timestamp| {
            let frame = self.frame_at(timestamp).ok()?;
            let frame_time = self.duration_for(frame).ok()?;
            (timestamp.abs_diff(frame_time) <= Duration::from_nanos(1)).then_some(frame)
        };
        frame_on_grid(origin)
            .zip(frame_on_grid(time))
            .and_then(|(origin, time)| self.duration_for(time.saturating_sub(origin)).ok())
            .unwrap_or_else(|| time.saturating_sub(origin))
    }

    /// Convert an exact interleaved sample count to complete frames.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] for zero channels or an incomplete frame.
    pub fn frame_count(self, samples: SampleCount) -> Result<FrameCount, SignalError> {
        let channels = self.channel_count()?.get();
        if !samples.get().is_multiple_of(channels) {
            return Err(SignalError::IncompleteFrame {
                channels,
                samples: samples.get(),
            });
        }
        Ok(FrameCount::new(samples.get() / channels))
    }

    /// Convert duration to whole frames without saturation.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when the frame count is not representable.
    pub fn frames_for(self, duration: Duration) -> Result<FrameCount, SignalError> {
        time::frames_for(self.sample_rate, duration).map(FrameCount::new)
    }

    /// Convert frames to interleaved samples without saturating.
    ///
    /// # Errors
    ///
    /// Returns [`SignalError`] when channels are zero or sample math overflows.
    pub fn sample_count(self, frames: FrameCount) -> Result<SampleCount, SignalError> {
        let channels = self.channel_count()?.get();
        frames
            .get()
            .checked_mul(channels)
            .map(SampleCount::new)
            .ok_or_else(|| SignalError::SampleCountOverflow {
                channels,
                frames: frames.get(),
            })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{consts, rescale_frame_ceil, rescale_frame_round};

    #[kithara::test]
    #[case::zero(0, 44_100, 48_000, Some(0))]
    #[case::up(1024, 44_100, 48_000, Some(1115))]
    #[case::down(1024, 48_000, 44_100, Some(941))]
    #[case::tie(1, 2, 1, Some(1))]
    #[case::below_tie(1, 3, 1, Some(0))]
    #[case::maximum(u64::MAX, u32::MAX, u32::MAX, Some(u64::MAX))]
    #[case::overflow(u64::MAX, 1, 2, None)]
    fn frame_rescaling_rounds_without_duration_quantization(
        #[case] frames: u64,
        #[case] source_rate: u32,
        #[case] target_rate: u32,
        #[case] expected: Option<u64>,
    ) {
        assert_eq!(
            rescale_frame_round(
                frames,
                NonZeroU32::new(source_rate).expect("source rate"),
                NonZeroU32::new(target_rate).expect("target rate"),
            ),
            expected,
        );
    }

    #[kithara::test]
    fn checked_shape_and_time_math() {
        let spec = AudioSpec::new(2, consts::INTERLEAVED_RATE);
        let duration = Duration::from_millis(12);

        assert_eq!(
            spec.sample_count(FrameCount::new(576)),
            Ok(SampleCount::new(1_152))
        );
        assert_eq!(
            spec.frame_count(SampleCount::new(1_152)),
            Ok(FrameCount::new(576))
        );
        assert_eq!(spec.duration_for(576), Ok(duration));
        assert_eq!(spec.frames_for(duration), Ok(FrameCount::new(576)));
        assert_eq!(spec.frame_at(duration), Ok(576));
        for (frame, source_rate, target_rate, expected) in [
            (0, 44_100, 48_000, 0),
            (44_100, 44_100, 48_000, 48_000),
            (44_101, 44_100, 48_000, 48_002),
            (3, 191_999, 191_997, 3),
            (95_997, 191_999, 192_001, 95_998),
            (u64::MAX, u32::MAX, u32::MAX, u64::MAX),
        ] {
            assert_eq!(
                rescale_frame_ceil(
                    frame,
                    NonZeroU32::new(source_rate).expect("source rate"),
                    NonZeroU32::new(target_rate).expect("target rate"),
                ),
                Some(expected),
            );
        }
    }

    #[kithara::test]
    fn elapsed_frame_time_matches_the_frame_difference() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
        let time = |frame| spec.duration_for(frame).expect("frame time");
        for (origin, frame) in [(1_024, 1_025), (1_024, 1_536), (0, 512), (1_105, 45_000)] {
            assert_eq!(
                spec.elapsed_since(time(origin), time(frame)),
                time(frame - origin),
                "frame {frame} after origin {origin}",
            );
        }
        assert_eq!(
            spec.elapsed_since(time(1_024), time(1_536) - Duration::from_nanos(1)),
            time(512),
            "a time one nanosecond short of a frame is that frame's time",
        );
        let between = time(1_536) + Duration::from_nanos(10_000);
        assert_eq!(
            spec.elapsed_since(time(1_024), between),
            between - time(1_024),
            "a time between frames keeps its nanoseconds",
        );
        assert_eq!(spec.elapsed_since(time(512), time(256)), Duration::ZERO);
    }

    #[kithara::test]
    fn elapsed_frame_time_normalizes_endpoints_before_subtracting() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
        let origin = spec.duration_for(1_024).expect("content origin");
        let raw = Duration::from_secs_f64(2_560.0 / 44_100.0);
        assert_eq!(raw - origin, Duration::from_nanos(34_829_933));
        assert_eq!(
            spec.elapsed_since(origin, raw),
            spec.duration_for(1_536).expect("content frame time")
        );
        for offset in [Duration::from_nanos(5_669), Duration::from_nanos(11_338)] {
            assert_eq!(
                spec.elapsed_since(origin, raw + offset),
                raw + offset - origin,
                "fractional-frame positions must retain their source phase"
            );
        }
    }

    #[kithara::test]
    fn elapsed_frame_time_accepts_floor_and_rounded_endpoints() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
        let floor = |frame| spec.duration_for(frame).expect("frame time");
        let rounded = |frame: u32| Duration::from_secs_f64(f64::from(frame) / 44_100.0);
        for (origin, end) in [(0, 512), (1_024, 2_560), (1_105, 45_000)] {
            for origin_time in [floor(u64::from(origin)), rounded(origin)] {
                for end_time in [floor(u64::from(end)), rounded(end)] {
                    assert_eq!(
                        spec.elapsed_since(origin_time, end_time),
                        floor(u64::from(end - origin))
                    );
                }
            }
        }
    }

    #[kithara::test]
    fn elapsed_frame_time_keeps_unrepresentable_endpoints_safe() {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("sample rate"));
        for (origin, end) in [
            (Duration::ZERO, Duration::MAX),
            (Duration::MAX, Duration::ZERO),
            (Duration::MAX, Duration::MAX),
            (Duration::from_nanos(u64::MAX), Duration::MAX),
        ] {
            assert_eq!(spec.elapsed_since(origin, end), end.saturating_sub(origin));
        }
        let end = spec
            .frame_at(Duration::from_nanos(u64::MAX - 100_000))
            .expect("representable endpoint");
        let origin = end - 1_024;
        assert_eq!(
            spec.elapsed_since(
                spec.duration_for(origin).expect("origin time"),
                spec.duration_for(end).expect("end time")
            ),
            spec.duration_for(1_024).expect("elapsed frame time")
        );
    }

    #[kithara::test]
    fn invalid_shape_is_typed() {
        let zero = AudioSpec::new(0, consts::INTERLEAVED_RATE);
        assert_eq!(zero.channel_count(), Err(SignalError::ChannelCountZero));

        let stereo = AudioSpec::new(2, consts::INTERLEAVED_RATE);
        assert_eq!(
            stereo.frame_count(SampleCount::new(3)),
            Err(SignalError::IncompleteFrame {
                samples: 3,
                channels: 2,
            })
        );
    }

    #[kithara::test]
    fn count_and_time_overflow_is_typed() {
        assert_eq!(
            rescale_frame_ceil(
                u64::MAX,
                NonZeroU32::MIN,
                NonZeroU32::new(2).expect("target rate"),
            ),
            None,
        );
        let stereo = AudioSpec::new(2, consts::INTERLEAVED_RATE);

        assert_eq!(
            stereo.sample_count(FrameCount::new(usize::MAX)),
            Err(SignalError::SampleCountOverflow {
                frames: usize::MAX,
                channels: 2,
            })
        );
        assert!(matches!(
            stereo.duration_for(u64::MAX),
            Err(SignalError::DurationOverflow { .. })
        ));
        assert!(matches!(
            stereo.frames_for(Duration::MAX),
            Err(SignalError::FrameCountOverflow { .. })
        ));
        assert!(matches!(
            stereo.frame_at(Duration::MAX),
            Err(SignalError::FrameCountOverflow { .. })
        ));
    }
}
