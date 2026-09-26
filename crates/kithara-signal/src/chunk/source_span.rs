use std::{
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
};

/// Decoded-source interval represented by a physical output interval.
///
/// Slices retain the exact rational source position and slope. Integer source
/// endpoints are rounded down only when queried, never used as a new basis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceSpan {
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    denominator: NonZeroU64,
    #[field(get, copy, with)]
    mapping_revision: Option<NonZeroU64>,
    numerator: u128,
    #[field(get, copy)]
    output_frames: u64,
    #[field(get, copy, with)]
    render_revision: u64,
    step: u64,
}

impl SourceSpan {
    /// Creates a mapping for a nonempty physical output interval.
    #[must_use]
    pub fn new(start: u64, end: u64, sample_rate: NonZeroU32, output_frames: u64) -> Option<Self> {
        let source_frames = end.checked_sub(start)?;
        let mut divisor = source_frames;
        let mut remainder = output_frames;
        while remainder != 0 {
            (divisor, remainder) = (remainder, divisor % remainder);
        }
        let denominator = NonZeroU64::new(output_frames.checked_div(divisor)?)?;
        Some(Self {
            numerator: u128::from(start) * u128::from(denominator.get()),
            step: source_frames / divisor,
            denominator,
            output_frames,
            sample_rate,
            render_revision: 0,
            mapping_revision: None,
        })
    }

    /// Exclusive decoded-source frame, rounded down on the source lattice.
    ///
    /// # Panics
    /// Panics if the private validated mapping invariant is violated.
    #[must_use]
    pub fn end(self) -> u64 {
        u64::try_from(
            (self.numerator + u128::from(self.step) * u128::from(self.output_frames))
                / u128::from(self.denominator.get()),
        )
        .expect("validated source mapping ends within u64")
    }

    /// Joins adjacent output intervals only when their exact mappings agree.
    #[must_use]
    pub fn followed_by(self, next: Self) -> Option<Self> {
        let boundary = self
            .numerator
            .checked_add(u128::from(self.step) * u128::from(self.output_frames))?;
        if self.step != next.step
            || self.denominator != next.denominator
            || boundary != next.numerator
            || self.sample_rate != next.sample_rate
            || self.render_revision != next.render_revision
            || self.mapping_revision != next.mapping_revision
        {
            return None;
        }
        let output_frames = self.output_frames.checked_add(next.output_frames)?;
        self.numerator
            .checked_add(u128::from(self.step) * u128::from(output_frames))?;
        Some(Self {
            output_frames,
            ..self
        })
    }

    /// Slices relative output frames without rounding the retained source basis.
    #[must_use]
    pub fn for_output_range(self, range: Range<u64>) -> Option<Self> {
        if range.start > range.end || range.end > self.output_frames {
            return None;
        }
        Some(Self {
            numerator: self
                .numerator
                .checked_add(u128::from(self.step) * u128::from(range.start))?,
            output_frames: range.end - range.start,
            ..self
        })
    }

    /// Inclusive decoded-source frame, rounded down on the source lattice.
    ///
    /// # Panics
    /// Panics if the private validated mapping invariant is violated.
    ///
    /// Constructors accept `u64` endpoints, slicing stays within them, and joining requires the
    /// exact boundary of another validated interval — together they uphold the private mapping
    /// invariant this panics on.
    #[must_use]
    pub fn start(self) -> u64 {
        u64::try_from(self.numerator / u128::from(self.denominator.get()))
            .expect("validated source mapping starts within u64")
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn nested_source_slices_keep_the_original_rational_phase() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for origin in [0, 100, u64::MAX - 192] {
            let span = SourceSpan::new(origin, origin + 192, rate, 128).expect("span");
            let nested = span
                .for_output_range(0..127)
                .expect("prefix")
                .for_output_range(0..2)
                .expect("nested prefix");
            assert_eq!(nested.end(), origin + 3);
            assert_eq!(Some(nested), span.for_output_range(0..2));
            let first = span.for_output_range(0..1).expect("first");
            let rest = span.for_output_range(1..128).expect("rest");
            assert_eq!(first.followed_by(rest), Some(span));
        }
    }

    #[kithara::test]
    fn rounded_endpoints_do_not_authorize_a_different_mapping_join() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let original = SourceSpan::new(0, 192, rate, 128).expect("span");
        let first = original.for_output_range(0..1).expect("first");
        let rounded = SourceSpan::new(1, 4, rate, 2).expect("same slope, wrong phase");
        assert!(first.followed_by(rounded).is_none());
    }
    #[kithara::test]
    fn source_span_requires_output_and_preserves_valid_source_boundaries() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        assert!(SourceSpan::new(0, 0, rate, 0).is_none());
        assert!(SourceSpan::new(0, 1, rate, 0).is_none());
        assert!(SourceSpan::new(2, 1, rate, 1).is_none());
        let standing = SourceSpan::new(u64::MAX, u64::MAX, rate, 128).expect("standing source");
        assert_eq!(standing.start(), u64::MAX);
        assert_eq!(standing.end(), u64::MAX);
        assert_eq!(
            standing.for_output_range(127..128).expect("suffix").end(),
            u64::MAX
        );
        assert!(standing.for_output_range(0..129).is_none());
        assert!(
            standing
                .for_output_range(Range { start: 2, end: 1 })
                .is_none()
        );
        let full = SourceSpan::new(0, u64::MAX, rate, u64::MAX).expect("full source");
        assert_eq!(full.end(), u64::MAX);
        assert_eq!(
            full.for_output_range(1..u64::MAX).expect("suffix").start(),
            1
        );
    }
}
