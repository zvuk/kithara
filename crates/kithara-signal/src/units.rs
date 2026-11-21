use std::{marker::PhantomData, num::NonZeroU32};

use num_traits::ToPrimitive;

/// A count tagged with its unit, without trait bounds on the unit marker.
#[derive_where::derive_where(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Count<Unit>(
    usize,
    #[derive_where(skip(Debug))] PhantomData<fn() -> Unit>,
);

/// The unit marker for audio frames.
pub enum Frames {}

/// The unit marker for interleaved samples.
pub enum Samples {}

/// A count of audio frames, with one sample per channel in each frame.
///
/// Frame and sample counts are distinct types.
///
/// ```compile_fail
/// use kithara_signal::{FrameCount, SampleCount};
/// let frames: FrameCount = SampleCount::new(1);
/// ```
pub type FrameCount = Count<Frames>;

/// A count of interleaved samples.
pub type SampleCount = Count<Samples>;

impl<Unit> Count<Unit> {
    /// Creates a count in the tagged unit.
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self(count, PhantomData)
    }

    /// Returns the number of units.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl FrameCount {
    /// Round single-precision seconds to frames, with half-frames rounded up.
    /// Negative or `NaN` time becomes zero; overflow saturates at [`usize::MAX`].
    #[must_use]
    pub fn for_seconds(seconds: f32, sample_rate: NonZeroU32) -> Self {
        let rate = sample_rate.get().to_f32().unwrap_or(f32::MAX);
        Self::new(
            (seconds.max(0.0) * rate)
                .round()
                .to_usize()
                .unwrap_or(usize::MAX),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::{Count, FrameCount, SampleCount};

    #[kithara::test]
    #[case::declick_half_frame(0.005, 44_100, 221)]
    #[case::half_frame(0.5, 1, 1)]
    #[case::below_half_frame(0.49, 1, 0)]
    #[case::zero(0.0, 48_000, 0)]
    #[case::negative(-1.0, 48_000, 0)]
    #[case::nan(f32::NAN, 48_000, 0)]
    #[case::negative_infinity(f32::NEG_INFINITY, 48_000, 0)]
    #[case::infinity(f32::INFINITY, 48_000, usize::MAX)]
    #[case::overflow(f32::MAX, u32::MAX, usize::MAX)]
    fn frame_counts_round_single_precision_seconds_and_saturate(
        #[case] seconds: f32,
        #[case] rate: u32,
        #[case] expected: usize,
    ) {
        assert_eq!(
            FrameCount::for_seconds(seconds, NonZeroU32::new(rate).expect("rate")).get(),
            expected
        );
    }

    #[kithara::test]
    fn counts_keep_value_traits_without_marker_bounds() {
        enum Unit {}

        fn assert_traits<Value: Clone + Copy + std::fmt::Debug + Default + Eq + Ord>() {}
        const COUNT: Count<Unit> = Count::new(3);
        const VALUE: usize = COUNT.get();

        assert_traits::<Count<Unit>>();
        assert_traits::<FrameCount>();
        assert_traits::<SampleCount>();

        assert_eq!(VALUE, 3);
        assert_eq!(COUNT, Count::new(3));
        assert_eq!(Count::<Unit>::default().get(), 0);
        assert_eq!(format!("{COUNT:?}"), "Count(3)");
        assert!(Count::<Unit>::default() < COUNT);
        assert_eq!(
            COUNT.partial_cmp(&Count::new(4)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(size_of::<Count<Unit>>(), size_of::<usize>());
    }
}
