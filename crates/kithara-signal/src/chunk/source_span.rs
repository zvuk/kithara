use std::{
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    ops::Range,
};

use kithara_platform::time::Duration;
use num_traits::ToPrimitive;

/// Decoded-source interval represented by a physical output interval.
///
/// Slices retain the exact rational source position and slope. Integer source
/// endpoints are rounded down only when queried, never used as a new basis.
/// Coefficients retain 128-bit precision when a curve and correction share a basis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceSpan {
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    denominator: NonZeroU128,
    #[field(get, copy, with)]
    mapping_revision: Option<NonZeroU64>,
    numerator: u128,
    #[field(get, copy)]
    output_frames: u64,
    #[field(get, copy, with)]
    render_revision: u64,
    step: u128,
    step_change: i128,
}

fn fractional_nanos(numerator: u128, denominator: u128) -> u32 {
    let mut remainder = 0;
    let mut nanos = 0;
    for bit in (0..u32::BITS).rev() {
        let complement = denominator - remainder;
        let mut carry = if remainder >= complement {
            remainder -= complement;
            1
        } else {
            remainder += remainder;
            0
        };
        if 1_000_000_000u32 & (1 << bit) != 0 {
            let complement = denominator - numerator;
            if remainder >= complement {
                remainder -= complement;
                carry += 1;
            } else {
                remainder += numerator;
            }
        }
        nanos = nanos * 2 + carry;
    }
    nanos
}

impl SourceSpan {
    /// Source position in seconds at an output boundary, without nanosecond truncation.
    #[must_use]
    pub fn seconds_at(self, output_frame: u64) -> Option<f64> {
        let (numerator, denominator) = self.source_ratio_at(output_frame)?;
        Some(numerator.to_f64()? / denominator.get().to_f64()? / f64::from(self.sample_rate.get()))
    }

    /// Source position at an output boundary, retaining rational phase.
    #[must_use]
    pub fn position_at(self, output_frame: u64) -> Option<Duration> {
        let (numerator, denominator) = self.source_ratio_at(output_frame)?;
        let frames = numerator / denominator.get();
        let rate = u128::from(self.sample_rate.get());
        let seconds = u64::try_from(frames / rate).ok()?;
        let fraction = fractional_nanos(numerator % denominator.get(), denominator.get());
        let nanos =
            u32::try_from(((frames % rate) * 1_000_000_000 + u128::from(fraction)) / rate).ok()?;
        Some(Duration::new(seconds, nanos))
    }

    /// Creates a mapping for a nonempty physical output interval.
    #[must_use]
    pub fn new(start: u64, end: u64, sample_rate: NonZeroU32, output_frames: u64) -> Option<Self> {
        let source_frames = end.checked_sub(start)?;
        let mut divisor = source_frames;
        let mut remainder = output_frames;
        while remainder != 0 {
            (divisor, remainder) = (remainder, divisor % remainder);
        }
        let denominator = NonZeroU128::new(u128::from(output_frames.checked_div(divisor)?))?;
        Self::try_from((
            u128::from(start) * denominator.get(),
            u128::from(source_frames / divisor),
            denominator,
            sample_rate,
            output_frames,
        ))
        .ok()
    }

    fn step_at(self, frame: u64) -> Option<u128> {
        let change = self
            .step_change
            .unsigned_abs()
            .checked_mul(u128::from(frame))?;
        if self.step_change < 0 {
            self.step.checked_sub(change)
        } else {
            self.step.checked_add(change)
        }
    }

    fn numerator_at(self, frame: u64) -> Option<u128> {
        let linear = self.step.checked_mul(u128::from(frame))?;
        let pairs = u128::from(frame).checked_mul(u128::from(frame.saturating_sub(1)))? / 2;
        let change = pairs.checked_mul(self.step_change.unsigned_abs())?;
        let advance = if self.step_change < 0 {
            linear.checked_sub(change)?
        } else {
            linear.checked_add(change)?
        };
        self.numerator.checked_add(advance)
    }

    /// Exact reduced decoded-source coordinate at an output boundary.
    #[must_use]
    pub fn source_ratio_at(self, output_frame: u64) -> Option<(u128, NonZeroU128)> {
        if output_frame > self.output_frames {
            return None;
        }
        let numerator = self.numerator_at(output_frame)?;
        let mut divisor = self.denominator.get();
        let mut remainder = numerator;
        while remainder != 0 {
            (divisor, remainder) = (remainder, divisor % remainder);
        }
        Some((
            numerator / divisor,
            NonZeroU128::new(self.denominator.get() / divisor)?,
        ))
    }

    /// Exclusive decoded-source frame, rounded down on the source lattice.
    ///
    /// # Panics
    /// Panics if the private validated mapping invariant is violated.
    #[must_use]
    pub fn end(self) -> u64 {
        let Some(end) = self
            .numerator_at(self.output_frames)
            .and_then(|numerator| u64::try_from(numerator / self.denominator.get()).ok())
        else {
            unreachable!("validated source mapping endpoint");
        };
        end
    }

    /// Joins adjacent output intervals only when their exact mappings agree.
    #[must_use]
    pub fn followed_by(self, next: Self) -> Option<Self> {
        let boundary = self.numerator_at(self.output_frames)?;
        if self.step_at(self.output_frames)? != next.step
            || self.step_change != next.step_change
            || self.denominator != next.denominator
            || boundary != next.numerator
            || self.sample_rate != next.sample_rate
            || self.render_revision != next.render_revision
            || self.mapping_revision != next.mapping_revision
        {
            return None;
        }
        let output_frames = self.output_frames.checked_add(next.output_frames)?;
        u64::try_from(self.numerator_at(output_frames)? / self.denominator.get()).ok()?;
        if output_frames > 0 {
            self.step_at(output_frames - 1)?;
        }
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
            numerator: self.numerator_at(range.start)?,
            step: if range.start == range.end {
                0
            } else {
                self.step_at(range.start)?
            },
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
        let Ok(start) = u64::try_from(self.numerator / self.denominator.get()) else {
            unreachable!("validated source mapping origin");
        };
        start
    }
}

impl TryFrom<(u128, u128, NonZeroU128, NonZeroU32, u64)> for SourceSpan {
    type Error = ();

    /// Creates a checked affine mapping with a fractional source origin.
    fn try_from(value: (u128, u128, NonZeroU128, NonZeroU32, u64)) -> Result<Self, Self::Error> {
        let (start_numerator, step_numerator, denominator, sample_rate, output_frames) = value;
        Self::try_from((
            start_numerator,
            step_numerator,
            0,
            denominator,
            sample_rate,
            output_frames,
        ))
    }
}

impl TryFrom<(u128, u128, i128, NonZeroU128, NonZeroU32, u64)> for SourceSpan {
    type Error = ();

    /// Creates a checked arithmetic progression of source-frame advances.
    fn try_from(
        value: (u128, u128, i128, NonZeroU128, NonZeroU32, u64),
    ) -> Result<Self, Self::Error> {
        let (start_numerator, step_numerator, step_change, denominator, sample_rate, output_frames) =
            value;
        if output_frames == 0 {
            return Err(());
        }
        let span = Self {
            numerator: start_numerator,
            step: step_numerator,
            step_change,
            denominator,
            output_frames,
            sample_rate,
            render_revision: 0,
            mapping_revision: None,
        };
        span.step_at(output_frames - 1).ok_or(())?;
        let end = span.numerator_at(output_frames).ok_or(())?;
        u64::try_from(end / denominator.get()).map_err(|_| ())?;
        Ok(span)
    }
}
