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
    use crate::consts;

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
