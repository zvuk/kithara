use std::{
    cmp::Ordering,
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    ops::Range,
};

use kithara_platform::time::Duration;
use num_traits::ToPrimitive;

use super::{
    affine::{Affine, Offset},
    approach::Approach,
    mapping::{Mapping, Rational},
    ratio::Ratio,
};
use crate::ExponentialApproach;

/// Decoded-source interval represented by a physical output interval.
///
/// Rational mappings retain exact coefficients. Exponential mappings retain
/// their original analytic basis and round advance to the nearest Q32 lattice.
/// Slices never replace either basis with rounded source endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct SourceSpan {
    #[field(get, copy)]
    sample_rate: NonZeroU32,
    #[field(get, copy, with)]
    mapping_revision: Option<NonZeroU64>,
    mapping: Mapping,
    #[field(get, copy)]
    output_frames: u64,
    #[field(get, copy, with)]
    render_revision: u64,
}

/// Raw source coordinate to physical output coordinate for one affine
/// [`SourceSpan`], built once by [`SourceSpan::source_inverse`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceInverse(Affine);

impl SourceInverse {
    /// Reduced output coordinate of a raw source position.
    ///
    /// Negative output coordinates and overflow return `None`.
    #[must_use]
    pub fn output_at(self, position: (u128, NonZeroU128)) -> Option<(u128, NonZeroU128)> {
        let position = self.0.apply(Ratio::new(position.0, position.1)?)?;
        Some((position.numerator, position.denominator))
    }
}

/// Exact source time from a media origin to a [`SourceSpan`] boundary.
///
/// Boundaries keep their rational phase. An origin within nanosecond
/// precision of a whole source frame counts as that frame; any other origin
/// counts as its exact timestamp. Spans of different sample rates compare on
/// one time axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceElapsed(Ratio);

impl Ord for SourceElapsed {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.compare(other.0)
    }
}

impl PartialOrd for SourceElapsed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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

    /// Exact source time from `origin` to an output boundary, saturating at zero.
    #[must_use]
    pub fn elapsed_at(self, output_frame: u64, origin: Duration) -> Option<SourceElapsed> {
        let rate = u128::from(self.sample_rate.get());
        let origin = match crate::time::frame_on_grid(self.sample_rate, origin) {
            Some(frame) => Ratio::new(u128::from(frame), NonZeroU128::MIN)?,
            None => Ratio::new(
                origin.as_nanos().checked_mul(rate)?,
                NonZeroU128::new(1_000_000_000)?,
            )?,
        };
        let (numerator, denominator) = self.source_ratio_at(output_frame)?;
        let position = Ratio::new(numerator, denominator)?;
        let frames = if position.compare(origin) == Ordering::Greater {
            position.sub(origin)?
        } else {
            Ratio::new(0, NonZeroU128::MIN)?
        };
        Some(SourceElapsed(
            frames.mul(Ratio::new(1, NonZeroU128::new(rate)?)?)?,
        ))
    }

    /// The output boundary in `[from, output_frames]` whose
    /// [`elapsed_at`](Self::elapsed_at) `origin` is nearest to `elapsed`; an
    /// exact midpoint rounds to the earlier boundary.
    ///
    /// Boundary `from` also owns the half step before it, sized by its own
    /// first output frame; an `elapsed` earlier than that rounds before the
    /// range and returns `Some(None)`. Every validated mapping is
    /// non-decreasing, so this inverts nonlinear spans too. An empty range and
    /// unrepresentable positions return `None`.
    #[must_use]
    pub fn output_nearest_since(
        self,
        from: u64,
        origin: Duration,
        elapsed: SourceElapsed,
    ) -> Option<Option<u64>> {
        if from >= self.output_frames {
            return None;
        }
        if self.rounds_before(from, origin, elapsed)? {
            return Some(None);
        }
        let reached = self.output_reaching(from, origin, elapsed)?;
        if reached == from {
            return Some(Some(from));
        }
        let below = self.elapsed_at(reached - 1, origin)?;
        if reached <= self.output_frames
            && elapsed
                .0
                .compare(Self::midpoint(below, self.elapsed_at(reached, origin)?)?)
                == Ordering::Greater
        {
            return Some(Some(reached));
        }
        self.output_reaching(from, origin, below).map(Some)
    }

    /// Whether `elapsed` lies before the half step that boundary `from` owns
    /// ahead of itself, sized by its first output frame.
    fn rounds_before(self, from: u64, origin: Duration, elapsed: SourceElapsed) -> Option<bool> {
        let first = self.elapsed_at(from, origin)?;
        let step = Self::midpoint(first, self.elapsed_at(from + 1, origin)?)?.sub(first.0)?;
        Some(
            step.compare(first.0) != Ordering::Greater
                && elapsed.0.compare(first.0.sub(step)?) == Ordering::Less,
        )
    }

    fn midpoint(low: SourceElapsed, high: SourceElapsed) -> Option<Ratio> {
        let half = Ratio::new(1, NonZeroU128::new(2)?)?;
        low.0.add(high.0.sub(low.0)?.mul(half)?)
    }

    /// The first output boundary in `[from, output_frames]` whose elapsed
    /// time reaches `elapsed`, or `output_frames + 1` when none does.
    fn output_reaching(self, from: u64, origin: Duration, elapsed: SourceElapsed) -> Option<u64> {
        let (mut reached, mut past) = (from, self.output_frames.checked_add(1)?);
        while reached < past {
            let mid = reached + (past - reached) / 2;
            if self.elapsed_at(mid, origin)? < elapsed {
                reached = mid + 1;
            } else {
                past = mid;
            }
        }
        Some(reached)
    }

    /// Maps an original exponential source basis over a nonempty output span.
    ///
    /// Rates are source frames per output frame; elapsed and the time constant
    /// are output frames. The rational origin is at the approach's own elapsed
    /// zero; a resumed approach reaches it already inside its course, so its
    /// advance counts from there.
    /// Unrepresentable coordinates or an overflowing range return `None`.
    #[must_use]
    pub fn approaching(
        origin_numerator: u128,
        origin_denominator: NonZeroU128,
        curve: ExponentialApproach,
        elapsed: u64,
        sample_rate: NonZeroU32,
        output_frames: u64,
    ) -> Option<Self> {
        if output_frames == 0 {
            return None;
        }
        let phase = curve.phase();
        let course = Approach {
            curve: curve.course(),
            elapsed: 0,
            origin: Ratio::new(origin_numerator, origin_denominator)?,
            affine: Affine::identity()?,
        };
        let mapping = Mapping::Approach(if phase == 0 {
            Approach { elapsed, ..course }
        } else {
            Approach {
                elapsed: elapsed.checked_add(phase)?,
                affine: Affine {
                    offset: Offset::Subtract(course.advance_at(phase)?),
                    ..course.affine
                },
                ..course
            }
        });
        mapping.validate(output_frames)?;
        Some(Self {
            mapping,
            output_frames,
            sample_rate,
            render_revision: 0,
            mapping_revision: None,
        })
    }

    /// Applies `position + scale * (source - pivot)` without changing the basis.
    ///
    /// All three coordinates are exact ratios. A positive scale is required;
    /// negative or unrepresentable source results return `None`.
    #[must_use]
    pub fn scaled_from(
        self,
        pivot: (u128, NonZeroU128),
        position: (u128, NonZeroU128),
        scale: (u128, NonZeroU128),
    ) -> Option<Self> {
        let affine = Affine::new(
            Ratio::new(pivot.0, pivot.1)?,
            Ratio::new(position.0, position.1)?,
            Ratio::new(scale.0, scale.1)?,
        )?;
        let mapping = self.mapping.transformed(affine)?;
        mapping.validate(self.output_frames)?;
        Some(Self { mapping, ..self })
    }

    /// Whether this mapping has a constant exact source advance.
    #[must_use]
    pub const fn is_affine(self) -> bool {
        matches!(
            self.mapping,
            Mapping::Rational(Rational { step_change: 0, .. })
        )
    }

    fn affine_basis(self) -> Option<(Ratio, Ratio)> {
        let Mapping::Rational(Rational {
            numerator,
            denominator,
            step,
            step_change: 0,
        }) = self.mapping
        else {
            return None;
        };
        Some((
            Ratio::new(numerator, denominator)?,
            Ratio::new(step, denominator)?,
        ))
    }

    /// Inverse of an affine source mapping around its physical output origin,
    /// for converting many raw source coordinates through one span.
    ///
    /// Nonlinear mappings and overflow return `None`.
    #[must_use]
    pub fn source_inverse(self, output_origin: u64) -> Option<SourceInverse> {
        let Mapping::Rational(Rational {
            numerator,
            denominator,
            step,
            step_change: 0,
        }) = self.mapping
        else {
            return None;
        };
        let step = NonZeroU128::new(step)?;
        Some(SourceInverse(Affine {
            scale: Ratio::new(denominator.get(), step)?,
            offset: Offset::difference(
                Ratio::new(u128::from(output_origin), NonZeroU128::MIN)?,
                Ratio::new(numerator, step)?,
            )?,
        }))
    }

    /// Inverts an affine source coordinate around its physical output origin.
    ///
    /// Nonlinear mappings, negative output coordinates and overflow return `None`.
    #[must_use]
    pub fn output_ratio_at_source(
        self,
        output_origin: u64,
        position: (u128, NonZeroU128),
    ) -> Option<(u128, NonZeroU128)> {
        self.source_inverse(output_origin)?.output_at(position)
    }

    /// Publishes a physical span through this affine input's source clock.
    ///
    /// The result retains the span's rate and analytic law; source coordinates
    /// are normalized to that rate. Unsupported or unrepresentable maps return `None`.
    #[must_use]
    pub fn map_output_span(self, output_origin: u64, span: Self) -> Option<Self> {
        let (origin, step) = self.affine_basis()?;
        let rate = Ratio::new(
            u128::from(span.sample_rate.get()),
            NonZeroU128::from(self.sample_rate),
        )?;
        let origin = origin.mul(rate)?;
        let step = step.mul(rate)?;
        span.scaled_from(
            (u128::from(output_origin), NonZeroU128::MIN),
            (origin.numerator, origin.denominator),
            (step.numerator, step.denominator),
        )
    }

    /// Repositions an affine input pivot without rounding its source clock.
    #[must_use]
    pub fn rebase_output(self, previous: u64, next: u64) -> Option<Self> {
        let (origin, step) = self.affine_basis()?;
        let advance = step.mul(Ratio::new(
            u128::from(previous.abs_diff(next)),
            NonZeroU128::MIN,
        )?)?;
        let position = if next < previous {
            origin.sub(advance)?
        } else {
            origin.add(advance)?
        };
        self.scaled_from(
            (origin.numerator, origin.denominator),
            (position.numerator, position.denominator),
            (1, NonZeroU128::MIN),
        )
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

    /// Each output frame's source coordinate in order, from output frame 0.
    #[must_use]
    pub fn source_steps(self) -> Option<super::SourceSteps> {
        super::SourceSteps::new(self.mapping)
    }

    /// Reduced source coordinate, exact for rational maps and Q32 for approaches.
    #[must_use]
    pub fn source_ratio_at(self, output_frame: u64) -> Option<(u128, NonZeroU128)> {
        if output_frame > self.output_frames {
            return None;
        }
        let position = self.mapping.ratio_at(output_frame)?;
        Some((position.numerator, position.denominator))
    }

    /// Exclusive decoded-source frame, rounded down on the source lattice.
    ///
    /// # Panics
    /// Panics if the private validated mapping invariant is violated.
    #[must_use]
    pub fn end(self) -> u64 {
        let Some(end) = self
            .mapping
            .ratio_at(self.output_frames)
            .and_then(Ratio::source_frame)
        else {
            unreachable!("validated source mapping endpoint");
        };
        end
    }

    /// Joins adjacent output intervals only when their exact mappings agree.
    #[must_use]
    pub fn followed_by(self, next: Self) -> Option<Self> {
        if self.sample_rate != next.sample_rate
            || self.render_revision != next.render_revision
            || self.mapping_revision != next.mapping_revision
        {
            return None;
        }
        let mapping = self.mapping.followed_by(next.mapping, self.output_frames)?;
        let output_frames = self.output_frames.checked_add(next.output_frames)?;
        mapping.validate(output_frames)?;
        Some(Self {
            mapping,
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
        if range.start == 0 && range.end == self.output_frames && self.output_frames != 0 {
            return Some(self);
        }
        let output_frames = range.end - range.start;
        let mapping = self.mapping.sliced(range.start, output_frames)?;
        mapping.validate(output_frames)?;
        Some(Self {
            mapping,
            output_frames,
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
        let Some(start) = self.mapping.ratio_at(0).and_then(Ratio::source_frame) else {
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
        let mapping = Mapping::Rational(Rational {
            numerator: start_numerator,
            step: step_numerator,
            step_change,
            denominator,
        });
        mapping.validate(output_frames).ok_or(())?;
        let span = Self {
            mapping,
            output_frames,
            sample_rate,
            render_revision: 0,
            mapping_revision: None,
        };
        Ok(span)
    }
}

fn split(value: u128) -> [u64; 2] {
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = value.to_le_bytes();
    [
        u64::from_le_bytes([a, b, c, d, e, f, g, h]),
        u64::from_le_bytes([i, j, k, l, m, n, o, p]),
    ]
}

fn joined(words: &[u64]) -> u128 {
    u128::from(words[0]) | (u128::from(words[1]) << 64)
}

fn write_ratio(words: &mut [u64], ratio: Ratio) {
    words[..2].copy_from_slice(&split(ratio.numerator));
    words[2..4].copy_from_slice(&split(ratio.denominator.get()));
}

fn read_ratio(words: &[u64]) -> Result<Ratio, ()> {
    Ratio::new(
        joined(words),
        NonZeroU128::new(joined(&words[2..])).ok_or(())?,
    )
    .ok_or(())
}

impl From<SourceSpan> for [u64; 24] {
    /// Encodes exact native geometry for bounded telemetry, retaining its analytic basis.
    fn from(span: SourceSpan) -> Self {
        let mut words = [0; 24];
        words[0] = 1;
        words[2] = u64::from(span.sample_rate.get());
        words[3] = span.output_frames;
        words[4] = span.render_revision;
        words[5] = span.mapping_revision.map_or(0, NonZeroU64::get);
        match span.mapping {
            Mapping::Rational(mapping) => {
                words[6..8].copy_from_slice(&split(mapping.numerator));
                words[8..10].copy_from_slice(&split(mapping.denominator.get()));
                words[10..12].copy_from_slice(&split(mapping.step));
                words[12..14].copy_from_slice(&split(u128::from_le_bytes(
                    mapping.step_change.to_le_bytes(),
                )));
            }
            Mapping::Approach(mapping) => {
                words[1] = 1;
                words[6] = mapping.curve.initial().to_bits();
                words[7] = mapping.curve.target().to_bits();
                words[8] = mapping.curve.time_constant().to_bits();
                words[9] = mapping.elapsed;
                write_ratio(&mut words[10..14], mapping.origin);
                write_ratio(&mut words[14..18], mapping.affine.scale);
                let (subtract, offset) = match mapping.affine.offset {
                    Offset::Add(offset) => (false, offset),
                    Offset::Subtract(offset) => (true, offset),
                };
                words[18] = u64::from(subtract);
                write_ratio(&mut words[19..23], offset);
            }
        }
        words
    }
}

fn read_approach(words: &[u64; 24]) -> Result<Approach, ()> {
    let offset = read_ratio(&words[19..23])?;
    let offset = match words[18] {
        0 => Offset::Add(offset),
        1 => Offset::Subtract(offset),
        _ => return Err(()),
    };
    let scale = read_ratio(&words[14..18])?;
    if scale.numerator == 0 {
        return Err(());
    }
    Ok(Approach {
        curve: ExponentialApproach::new(
            f64::from_bits(words[6]),
            f64::from_bits(words[7]),
            f64::from_bits(words[8]),
        )
        .ok_or(())?,
        elapsed: words[9],
        origin: read_ratio(&words[10..14])?,
        affine: Affine { scale, offset },
    })
}

impl TryFrom<[u64; 24]> for SourceSpan {
    type Error = ();

    /// Rejects malformed telemetry before it can become source geometry.
    fn try_from(words: [u64; 24]) -> Result<Self, Self::Error> {
        if words[0] != 1 {
            return Err(());
        }
        let sample_rate = NonZeroU32::new(u32::try_from(words[2]).map_err(|_| ())?).ok_or(())?;
        let mapping = match words[1] {
            0 => Mapping::Rational(Rational {
                numerator: joined(&words[6..8]),
                denominator: NonZeroU128::new(joined(&words[8..10])).ok_or(())?,
                step: joined(&words[10..12]),
                step_change: i128::from_le_bytes(joined(&words[12..14]).to_le_bytes()),
            }),
            1 => Mapping::Approach(read_approach(&words)?),
            _ => return Err(()),
        };
        mapping.validate(words[3]).ok_or(())?;
        let span = Self {
            sample_rate,
            mapping_revision: NonZeroU64::new(words[5]),
            mapping,
            output_frames: words[3],
            render_revision: words[4],
        };
        if <[u64; 24]>::from(span) != words {
            return Err(());
        }
        Ok(span)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn original_output_ratio_at_source(
        span: SourceSpan,
        output_origin: u64,
        position: (u128, NonZeroU128),
    ) -> Option<(u128, NonZeroU128)> {
        let Mapping::Rational(Rational {
            numerator,
            denominator,
            step,
            step_change: 0,
        }) = span.mapping
        else {
            return None;
        };
        let origin = Ratio::new(numerator, denominator)?;
        let step = Ratio::new(step, denominator)?;
        let inverse = Affine::new(
            origin,
            Ratio::new(u128::from(output_origin), NonZeroU128::MIN)?,
            Ratio::new(step.denominator.get(), NonZeroU128::new(step.numerator)?)?,
        )?;
        let position = inverse.apply(Ratio::new(position.0, position.1)?)?;
        Some((position.numerator, position.denominator))
    }

    #[kithara::test]
    fn affine_inverse_matches_original_full_width_arithmetic() {
        let values = [
            0,
            1,
            2,
            3,
            6,
            u128::from(u64::MAX),
            1 << 64,
            1 << 127,
            u128::MAX - 1,
            u128::MAX,
        ];
        let denominators = values[1..]
            .iter()
            .map(|&value| NonZeroU128::new(value).expect("positive denominator"))
            .collect::<Vec<_>>();
        for numerator in values {
            for &denominator in &denominators {
                for step in values {
                    let span = SourceSpan {
                        sample_rate: NonZeroU32::new(48_000).expect("rate"),
                        mapping_revision: None,
                        mapping: Mapping::Rational(Rational {
                            numerator,
                            denominator,
                            step,
                            step_change: 0,
                        }),
                        output_frames: 1,
                        render_revision: 0,
                    };
                    if let Some(step) = NonZeroU128::new(step) {
                        let origin = Ratio::new(numerator, denominator).expect("origin");
                        let advance = Ratio::new(step.get(), denominator).expect("advance");
                        let reciprocal = Ratio::new(
                            advance.denominator.get(),
                            NonZeroU128::new(advance.numerator).expect("positive advance"),
                        )
                        .expect("reciprocal");
                        assert_eq!(origin.mul(reciprocal), Ratio::new(numerator, step));
                    }
                    for output_origin in [0, u64::MAX] {
                        let inverse = span.source_inverse(output_origin);
                        for position_numerator in values {
                            for &position_denominator in &denominators {
                                let position = (position_numerator, position_denominator);
                                let expected =
                                    original_output_ratio_at_source(span, output_origin, position);
                                assert_eq!(
                                    span.output_ratio_at_source(output_origin, position),
                                    expected,
                                    "span={span:?}, output_origin={output_origin}, position={position:?}",
                                );
                                if let Some(inverse) = inverse {
                                    assert_eq!(inverse.output_at(position), expected);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn affine_inverse_preserves_exact_offsets_and_overflow_refusals() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for (numerator, step, denominator, output_origin, position, expected) in [
            (1, 2, 2, 3, (9, 2), Some((7, 1))),
            (10, 2, 1, 0, (12, 1), Some((1, 1))),
            (10, 2, 1, 7, (0, 1), Some((2, 1))),
            (10, 2, 1, 5, (0, 1), Some((0, 1))),
            (10, 2, 1, 0, (0, 1), None),
            (0, 3, 6, 0, (1, 4), Some((1, 2))),
            (
                0,
                u128::MAX,
                u128::MAX,
                0,
                (u128::MAX, 1),
                Some((u128::MAX, 1)),
            ),
            (u128::MAX, u128::MAX, u128::MAX, 0, (2, 1), Some((1, 1))),
            (u128::MAX - 1, 1, u128::MAX, 0, (1, 1), Some((1, 1))),
            (0, 1, u128::MAX, 0, (2, 1), None),
            (1, u128::MAX, 1, u64::MAX, (0, 1), None),
            (1, u128::MAX, 1, u64::MAX, (1, 1), None),
            (u128::MAX - 1, 1, u128::MAX - 1, 0, (2, 1), None),
            (0, 2, 1, 0, (1, u128::MAX), None),
            (0, 1, 1, u64::MAX, (u128::MAX, 1), None),
            (0, 1, 1, u64::MAX, (0, 1), Some((u128::from(u64::MAX), 1))),
            (0, 0, 1, 0, (0, 1), None),
        ] {
            let span = SourceSpan {
                sample_rate: rate,
                mapping_revision: None,
                mapping: Mapping::Rational(Rational {
                    numerator,
                    denominator: NonZeroU128::new(denominator).expect("denominator"),
                    step,
                    step_change: 0,
                }),
                output_frames: 1,
                render_revision: 0,
            };
            let position = (
                position.0,
                NonZeroU128::new(position.1).expect("position denominator"),
            );
            let expected = expected.map(|(numerator, denominator)| {
                (
                    numerator,
                    NonZeroU128::new(denominator).expect("expected denominator"),
                )
            });
            assert_eq!(
                span.output_ratio_at_source(output_origin, position),
                expected
            );
            assert_eq!(
                original_output_ratio_at_source(span, output_origin, position),
                expected,
            );
        }
    }

    #[kithara::test]
    fn admitted_affine_spans_preserve_intermediate_overflow_refusals() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for (numerator, step, denominator, output_origin, position) in [
            (1, u128::MAX - 1, u128::MAX, 2, (1, u128::MAX)),
            (u128::MAX - 2, 1, u128::MAX - 1, 0, (3, 2)),
        ] {
            let span = SourceSpan::try_from((
                numerator,
                step,
                NonZeroU128::new(denominator).expect("denominator"),
                rate,
                1,
            ))
            .expect("valid one-frame span");
            let position = (
                position.0,
                NonZeroU128::new(position.1).expect("position denominator"),
            );
            assert_eq!(span.output_ratio_at_source(output_origin, position), None);
            assert_eq!(
                original_output_ratio_at_source(span, output_origin, position),
                None,
            );
        }
    }

    #[kithara::test]
    #[case::affine(SourceSpan::try_from((3, 2, NonZeroU128::new(3).expect("denominator"), NonZeroU32::new(44_100).expect("rate"), 64)).expect("affine"), Duration::ZERO)]
    #[case::ramp(SourceSpan::try_from((0, 1, 1, NonZeroU128::MIN, NonZeroU32::new(48_000).expect("rate"), 64)).expect("ramp"), Duration::ZERO)]
    #[case::approach(SourceSpan::approaching(0, NonZeroU128::MIN, ExponentialApproach::new(1.0, 0.5, 16.0).expect("curve"), 0, NonZeroU32::new(48_000).expect("rate"), 64).expect("approach"), Duration::ZERO)]
    #[case::still(SourceSpan::try_from((5, 0, NonZeroU128::MIN, NonZeroU32::new(48_000).expect("rate"), 8)).expect("still"), Duration::ZERO)]
    #[case::whole_frame_origin(SourceSpan::new(49_025, 49_155, NonZeroU32::new(48_000).expect("rate"), 130).expect("affine"), Duration::from_nanos(21_333_333))]
    #[case::fractional_origin(SourceSpan::try_from((3, 2, NonZeroU128::new(3).expect("denominator"), NonZeroU32::new(44_100).expect("rate"), 64)).expect("affine"), Duration::from_nanos(7))]
    fn output_nearest_matches_the_closest_boundary_to_an_elapsed_time(
        #[case] span: SourceSpan,
        #[case] origin: Duration,
    ) {
        let frames = span.output_frames();
        let elapsed = (0..=frames)
            .map(|frame| span.elapsed_at(frame, origin).expect("elapsed").0)
            .collect::<Vec<_>>();
        let at = |frame: u64| elapsed[usize::try_from(frame).expect("frame")];
        let half = Ratio::new(1, NonZeroU128::new(2).expect("two")).expect("half");
        let instant = Ratio::new(
            1,
            NonZeroU128::new(1_000_000_000_000_000).expect("femtosecond"),
        )
        .expect("instant");
        let midpoints = elapsed.windows(2).map(|pair| {
            pair[0]
                .add(pair[1].sub(pair[0]).expect("step").mul(half).expect("half"))
                .expect("midpoint")
        });
        let probes = elapsed.iter().copied().chain(midpoints).flat_map(|time| {
            [time.sub(instant), Some(time), time.add(instant)]
                .into_iter()
                .flatten()
        });
        let distance = |first: Ratio, second: Ratio| {
            if first.compare(second) == Ordering::Less {
                second.sub(first)
            } else {
                first.sub(second)
            }
            .expect("distance")
        };
        for probe in probes {
            for from in 0..frames {
                let reach = at(from + 1)
                    .sub(at(from))
                    .expect("step")
                    .mul(half)
                    .expect("half step");
                let before = probe.add(reach).expect("reach").compare(at(from)) == Ordering::Less;
                let expected = (!before).then(|| {
                    (from..=frames)
                        .min_by(|&first, &second| {
                            distance(probe, at(first)).compare(distance(probe, at(second)))
                        })
                        .expect("nonempty range")
                });
                assert_eq!(
                    span.output_nearest_since(from, origin, SourceElapsed(probe)),
                    Some(expected),
                    "from {from}, elapsed {probe:?}"
                );
            }
        }
        assert_eq!(
            span.output_nearest_since(frames, origin, SourceElapsed(elapsed[0])),
            None
        );
    }

    /// Both spans follow one `step / denominator` law; the outgoing one
    /// starts at zero and the incoming one at origin frame 1024, so the
    /// incoming boundary after `incoming_start` is the outgoing `consumed`.
    /// The incoming origin may carry a sub-nanosecond residue on either side
    /// of the outgoing position; the matching boundary is still the nearest.
    #[kithara::test]
    #[case::whole_frame(1, 1, 48_002, 49_025, 0, 0)]
    #[case::fractional_phase(5, 4, 48_001, 61_024, 0, 0)]
    #[case::incoming_after_the_outgoing(1, 1, 48_002, 49_025, 0, 1)]
    #[case::incoming_before_the_outgoing(1, 1, 48_002, 49_025, 1, 0)]
    #[case::fractional_incoming_after_the_outgoing(5, 4, 48_001, 61_024, 0, 1)]
    fn an_origin_relative_boundary_match_is_the_nearest_boundary(
        #[case] step: u128,
        #[case] denominator: u128,
        #[case] consumed: u64,
        #[case] incoming_start: u128,
        #[case] outgoing_residue: u128,
        #[case] incoming_residue: u128,
    ) {
        const MICRO: u128 = 1_000_000;
        let rate = NonZeroU32::new(48_000).expect("rate");
        let denominator = NonZeroU128::new(denominator * MICRO).expect("denominator");
        let outgoing = SourceSpan::try_from((
            outgoing_residue,
            step * MICRO,
            denominator,
            rate,
            consumed + 1,
        ))
        .expect("outgoing");
        let incoming = SourceSpan::try_from((
            incoming_start * denominator.get() + incoming_residue,
            step * MICRO,
            denominator,
            rate,
            8,
        ))
        .expect("incoming");
        let origin = crate::time::duration_for(rate, 1_024).expect("origin");
        let elapsed = outgoing
            .elapsed_at(consumed, Duration::ZERO)
            .expect("consumed");

        assert_eq!(
            incoming.output_nearest_since(0, origin, elapsed),
            Some(Some(1))
        );
    }

    #[kithara::test]
    fn inverse_rejects_nonlinear_and_invalid_source_geometry() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let ramp = SourceSpan::try_from((0, 1, 1, NonZeroU128::MIN, rate, 2)).expect("ramp");
        let approach = SourceSpan::approaching(
            0,
            NonZeroU128::MIN,
            ExponentialApproach::new(1.0, 0.9, 240.0).expect("curve"),
            0,
            rate,
            2,
        )
        .expect("approach");
        let zero = SourceSpan::try_from((0, 0, NonZeroU128::MIN, rate, 1)).expect("zero step");
        for span in [ramp, approach, zero] {
            for output_origin in [0, u64::MAX] {
                assert_eq!(span.source_inverse(output_origin), None);
            }
        }
        for span in [
            ramp,
            approach,
            zero,
            ramp.for_output_range(1..1).expect("point"),
        ] {
            for output_origin in [0, u64::MAX] {
                let position = (
                    u128::MAX,
                    NonZeroU128::new(u128::MAX - 1).expect("denominator"),
                );
                assert_eq!(span.output_ratio_at_source(output_origin, position), None);
                assert_eq!(
                    original_output_ratio_at_source(span, output_origin, position),
                    None,
                );
            }
        }
        assert!(SourceSpan::try_from((u128::MAX, 1, NonZeroU128::MIN, rate, 1)).is_err());
        let mut malformed: [u64; 24] = zero.into();
        malformed[8] = 0;
        malformed[9] = 0;
        assert!(SourceSpan::try_from(malformed).is_err());
    }

    #[kithara::test]
    fn native_geometry_telemetry_keeps_fractional_and_analytic_bases() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let rational = SourceSpan::try_from((
            (1u128 << 65) + 5,
            9,
            -1,
            NonZeroU128::new(10).expect("denominator"),
            rate,
            4,
        ))
        .expect("fractional quadratic");
        let curve = ExponentialApproach::new(1.2, 0.9, 240.0).expect("curve");
        let approach = SourceSpan::approaching(
            30_001,
            NonZeroU128::new(3).expect("denominator"),
            curve,
            11,
            rate,
            17,
        )
        .expect("approach");
        let transformed = approach
            .scaled_from(
                (1, NonZeroU128::MIN),
                (0, NonZeroU128::MIN),
                (3, NonZeroU128::new(2).expect("scale")),
            )
            .expect("affine approach");
        let cases = [
            rational,
            approach,
            transformed,
            transformed.for_output_range(5..13).expect("slice"),
            transformed.for_output_range(5..5).expect("point"),
        ];
        for span in cases {
            let span = span
                .with_render_revision(7)
                .with_mapping_revision(NonZeroU64::new(13));
            let words: [u64; 24] = span.into();
            let restored = SourceSpan::try_from(words).expect("exact native wire");
            assert_eq!(restored, span);
            for frame in 0..=span.output_frames() {
                assert_eq!(restored.source_ratio_at(frame), span.source_ratio_at(frame));
            }
        }
        let words: [u64; 24] = rational.into();
        assert_eq!(
            &words[..14],
            &[1, 0, 48_000, 4, 0, 0, 5, 2, 10, 0, 9, 0, u64::MAX, u64::MAX]
        );
        for (field, value) in [(0, 2), (1, 2), (2, 0), (8, 0), (23, 1)] {
            let mut malformed = words;
            malformed[field] = value;
            assert!(
                SourceSpan::try_from(malformed).is_err(),
                "wire field {field}"
            );
        }
    }

    fn original_for_output_range(span: SourceSpan, range: Range<u64>) -> Option<SourceSpan> {
        if range.start > range.end || range.end > span.output_frames {
            return None;
        }
        let output_frames = range.end - range.start;
        let mapping = span.mapping.sliced(range.start, output_frames)?;
        mapping.validate(output_frames)?;
        Some(SourceSpan {
            mapping,
            output_frames,
            ..span
        })
    }

    fn assert_original_views(span: SourceSpan) {
        let frames = span.output_frames();
        let span = span
            .with_render_revision(7)
            .with_mapping_revision(NonZeroU64::new(13));
        assert_eq!(span.for_output_range(0..frames), Some(span));
        let ranges = [
            0..frames,
            0..frames.saturating_sub(1),
            frames.min(1)..frames,
            frames / 3..frames / 2,
            0..0,
            frames..frames,
            Range {
                start: u64::MAX,
                end: 0,
            },
            u64::MAX..u64::MAX,
        ];
        for range in ranges {
            let actual = span.for_output_range(range.clone());
            let expected = original_for_output_range(span, range.clone());
            assert_eq!(actual, expected, "span={span:?}, range={range:?}");
            assert_eq!(
                actual.map(<[u64; 24]>::from),
                expected.map(<[u64; 24]>::from),
                "wire geometry: span={span:?}, range={range:?}",
            );
            if let Some(view) = actual {
                for frame in [0, view.output_frames() / 2, view.output_frames()] {
                    assert_eq!(
                        view.source_ratio_at(frame),
                        span.source_ratio_at(range.start + frame),
                    );
                    assert_eq!(
                        view.position_at(frame),
                        span.position_at(range.start + frame),
                    );
                }
            }
        }
        if let Some(past) = frames.checked_add(1) {
            assert_eq!(span.for_output_range(0..past), None);
            assert_eq!(span.for_output_range(past..past), None);
        }
    }

    #[kithara::test]
    fn full_range_views_preserve_admitted_rational_and_ramp_geometry() {
        let rate = NonZeroU32::new(192_000).expect("rate");
        let values = [0, 1, 2, u128::from(u64::MAX), 1 << 96, u128::MAX];
        let mut admitted = 0;
        for numerator in values {
            for step in values {
                for denominator in [1, 2, 1 << 96, u128::MAX] {
                    for change in [i128::MIN, -2, -1, 0, 1, 2, i128::MAX] {
                        for frames in [1, 2, 8, u64::MAX] {
                            if let Ok(span) = SourceSpan::try_from((
                                numerator,
                                step,
                                change,
                                NonZeroU128::new(denominator).expect("denominator"),
                                rate,
                                frames,
                            )) {
                                admitted += 1;
                                assert_original_views(span);
                            }
                        }
                    }
                }
            }
        }
        assert!(admitted > 0, "the matrix must exercise admitted mappings");
    }

    #[kithara::test]
    fn full_range_views_preserve_approach_and_transformed_geometry() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for (initial, target, time_constant) in [
            (1.2, 0.9, 240.0),
            (0.9, 1.2, 240.0),
            (0.5, 0.5, 240.0),
            (1.2, 0.9, 0.0),
            (1.2, 0.9, -0.0),
        ] {
            let curve = ExponentialApproach::new(initial, target, time_constant).expect("curve");
            for (origin, denominator, elapsed) in [
                ((1 << 100) + 1, 1 << 90, 0),
                ((1 << 100) + 1, 1 << 90, 11),
                (30_001, 3, u64::MAX / 2),
            ] {
                let span = SourceSpan::approaching(
                    origin,
                    NonZeroU128::new(denominator).expect("origin denominator"),
                    curve,
                    elapsed,
                    rate,
                    8,
                )
                .expect("admitted approach");
                assert_original_views(span);
                let transformed = span
                    .scaled_from(
                        (1, NonZeroU128::MIN),
                        (0, NonZeroU128::MIN),
                        (1, NonZeroU128::new(2).expect("scale")),
                    )
                    .expect("transformed approach");
                assert_original_views(transformed);
                assert_original_views(transformed.for_output_range(1..7).expect("nested approach"));
            }
        }
        let ramp = SourceSpan::try_from((
            257,
            33,
            -1,
            NonZeroU128::new(64).expect("denominator"),
            rate,
            8,
        ))
        .expect("ramp");
        assert_original_views(
            ramp.scaled_from(
                (1, NonZeroU128::MIN),
                (0, NonZeroU128::MIN),
                (3, NonZeroU128::new(2).expect("scale")),
            )
            .expect("transformed ramp"),
        );
    }

    #[kithara::test]
    fn empty_full_range_normalizes_deserialized_ramp_step() {
        let rate = NonZeroU32::new(48_000).expect("rate");
        for change in [i128::MIN, -2, 1, i128::MAX] {
            let ramp = SourceSpan::try_from((0, 3, change, NonZeroU128::MIN, rate, 1))
                .expect("one valid ramp advance");
            let mut words: [u64; 24] = ramp.into();
            words[3] = 0;
            let empty =
                SourceSpan::try_from(words).expect("admitted empty ramp with a nonzero step");
            assert_eq!(<[u64; 24]>::from(empty), words);
            let normalized = empty.for_output_range(0..0).expect("empty full range");
            assert_ne!(normalized, empty);
            assert_eq!(Some(normalized), original_for_output_range(empty, 0..0));
            words[10] = 0;
            words[11] = 0;
            assert_eq!(<[u64; 24]>::from(normalized), words);
            assert_eq!(SourceSpan::try_from(words), Ok(normalized));
            assert_eq!(normalized.source_ratio_at(0), empty.source_ratio_at(0));
            assert_eq!(normalized.for_output_range(0..0), Some(normalized));
            assert_eq!(empty.for_output_range(0..1), None);
        }
    }

    #[kithara::test]
    #[case::inside_the_source(30_001, 3)]
    #[case::at_a_fresh_segment(0, 1)]
    fn a_resumed_approach_continues_its_course_frame_for_frame(
        #[case] numerator: u128,
        #[case] denominator: u128,
    ) {
        let rate = NonZeroU32::new(48_000).expect("rate");
        let origin = Ratio::new(
            numerator,
            NonZeroU128::new(denominator).expect("denominator"),
        )
        .expect("origin");
        let curve = ExponentialApproach::new(1.0, 1.0333333333333334, 240.0).expect("curve");
        let (entered, tail) = (383, 512);
        let course = SourceSpan::approaching(
            origin.numerator,
            origin.denominator,
            curve,
            0,
            rate,
            entered + tail,
        )
        .expect("whole course");
        let resumed = SourceSpan::approaching(
            origin.numerator,
            origin.denominator,
            curve.resumed(entered).expect("resumed course"),
            0,
            rate,
            tail,
        )
        .expect("a resumed course starts at any origin");
        let words: [u64; 24] = resumed.into();
        assert_eq!(SourceSpan::try_from(words), Ok(resumed));
        let at = |span: SourceSpan, frame| {
            let (numerator, denominator) = span.source_ratio_at(frame).expect("source ratio");
            Ratio::new(numerator, denominator).expect("ratio")
        };
        let lead = at(course, entered).sub(origin).expect("course lead");
        for frame in 0..=tail {
            assert_eq!(
                at(resumed, frame).add(lead),
                Some(at(course, entered + frame)),
                "frame {frame}"
            );
        }
    }
}
