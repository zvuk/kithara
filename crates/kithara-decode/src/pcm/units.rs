use std::num::NonZeroUsize;

/// A count of PCM frames — one sample per channel.
///
/// Planar buffers are sized in frames, interleaved ones in [`Samples`], and the
/// two differ by the channel count. Keeping them apart is worth a type because
/// they are both `usize` and a buffer sized in the wrong one is silent: it
/// under- or over-reads by exactly the channel count.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Frames(usize);

/// A count of interleaved PCM samples — [`Frames`] times the channel count.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Samples(usize);

impl Frames {
    #[must_use]
    pub const fn new(frames: usize) -> Self {
        Self(frames)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// How many interleaved samples this many frames occupy.
    #[must_use]
    pub const fn samples(self, channels: NonZeroUsize) -> Samples {
        Samples(self.0.saturating_mul(channels.get()))
    }
}

impl Samples {
    #[must_use]
    pub const fn new(samples: usize) -> Self {
        Self(samples)
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// How many whole frames this many interleaved samples cover. A trailing
    /// partial frame is not a frame.
    #[must_use]
    pub const fn frames(self, channels: NonZeroUsize) -> Frames {
        Frames(self.0 / channels.get())
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    const STEREO: NonZeroUsize = NonZeroUsize::new(2).expect("2 is non-zero");

    #[kithara::test]
    fn frames_and_samples_differ_by_the_channel_count() {
        assert_eq!(Frames::new(128).samples(STEREO), Samples::new(256));
        assert_eq!(Samples::new(256).frames(STEREO), Frames::new(128));
    }

    #[kithara::test]
    fn a_trailing_partial_frame_is_not_a_frame() {
        assert_eq!(Samples::new(257).frames(STEREO), Frames::new(128));
    }

    #[kithara::test]
    fn a_frame_count_that_would_overflow_saturates_rather_than_wrapping() {
        assert_eq!(
            Frames::new(usize::MAX).samples(STEREO),
            Samples::new(usize::MAX)
        );
    }
}
