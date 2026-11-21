use std::num::NonZeroU32;

use kithara_signal::rescale_frame_round;

use crate::{DecodeError, DecodeResult};

/// Gapless trim contract for one decoded track.
///
/// The values are expressed in PCM frames, not scalar samples:
/// `leading_frames` are dropped from the start of the track and
/// `trailing_frames` are dropped from the end at EOF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct GaplessInfo {
    pub leading_frames: u64,
    pub trailing_frames: u64,
}

impl GaplessInfo {
    #[must_use]
    pub const fn new(leading_frames: u64, trailing_frames: u64) -> Self {
        Self {
            leading_frames,
            trailing_frames,
        }
    }

    /// The same trim counted in frames of `target_rate` instead of
    /// `source_rate`; `None` on overflow.
    #[must_use]
    pub fn rescaled(self, source_rate: NonZeroU32, target_rate: NonZeroU32) -> Option<Self> {
        Some(Self {
            leading_frames: rescale_frame_round(self.leading_frames, source_rate, target_rate)?,
            trailing_frames: rescale_frame_round(self.trailing_frames, source_rate, target_rate)?,
        })
    }
}

/// Trim a container declares, counted in frames of the rate the declaration
/// uses: the sample-entry rate of an MP4 track, the stream rate of MP3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeclaredGapless {
    pub(crate) info: GaplessInfo,
    pub(crate) rate: NonZeroU32,
}

impl DeclaredGapless {
    /// The trim in frames of a track reported at `rate`.
    pub(crate) fn at(self, rate: u32) -> DecodeResult<GaplessInfo> {
        let rate = NonZeroU32::new(rate).ok_or(DecodeError::InvalidSampleRate {
            resource: "gapless.track",
        })?;
        self.info
            .rescaled(self.rate, rate)
            .ok_or(DecodeError::InvalidData {
                detail: "gapless track-rate frame count overflow",
            })
    }
}

/// Ideal decoder-output frame count before gapless trim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GaplessTailCompensation {
    ideal_pre_trim_frames: u64,
}

impl GaplessTailCompensation {
    #[must_use]
    pub const fn new(ideal_pre_trim_frames: u64) -> Self {
        Self {
            ideal_pre_trim_frames,
        }
    }

    #[must_use]
    pub const fn deficit_frames(self, actual_pre_trim_frames: u64) -> u64 {
        self.ideal_pre_trim_frames
            .saturating_sub(actual_pre_trim_frames)
    }

    #[must_use]
    pub fn for_source_frames(
        source_frames: u64,
        source_rate: u32,
        output_rate: u32,
    ) -> Option<Self> {
        if source_rate == 0 || output_rate == 0 {
            return None;
        }
        let numerator = u128::from(source_frames).saturating_mul(u128::from(output_rate));
        let denominator = u128::from(source_rate);
        let ideal = numerator
            .saturating_add(denominator.saturating_sub(1))
            .saturating_div(denominator);
        u64::try_from(ideal).ok().map(Self::new)
    }

    #[must_use]
    pub const fn ideal_pre_trim_frames(self) -> u64 {
        self.ideal_pre_trim_frames
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::GaplessTailCompensation;

    #[kithara::test]
    fn tail_compensation_uses_ceil_resampled_source_frames() {
        let compensation = GaplessTailCompensation::for_source_frames(265_216, 44_100, 48_000)
            .expect("test rates are non-zero");

        assert_eq!(compensation.ideal_pre_trim_frames(), 288_671);
        assert_eq!(compensation.deficit_frames(288_670), 1);
    }
}
