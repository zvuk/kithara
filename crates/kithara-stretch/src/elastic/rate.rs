use std::ops::RangeInclusive;

use num_traits::Float;

use super::{ElasticError, ElasticRequest};

// i32-bounded numerators and denominators need fewer than 47 continued-fraction steps.
const RATE_FRACTION_DEPTH: u8 = 64;

/// Supported source-frame advance per output frame.
///
/// The envelope is the configured playback-rate policy restricted to ratios
/// representable by the prepared source and output frame limits.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct ElasticRateEnvelope {
    /// Maximum source-frame advance per output frame.
    #[field(get, copy)]
    max_source_frames_per_output: f64,
    /// Minimum source-frame advance per output frame.
    #[field(get, copy)]
    min_source_frames_per_output: f64,
}

impl ElasticRateEnvelope {
    pub(crate) fn contains(self, request: ElasticRequest) -> bool {
        request
            .source_frames_per_output()
            .is_ok_and(|rate| self.contains_rate(rate))
    }

    /// Returns whether a continuous source advance is supported.
    #[must_use]
    pub fn contains_rate(self, source_frames_per_output: f64) -> bool {
        source_frames_per_output.is_finite()
            && source_frames_per_output >= self.min_source_frames_per_output.next_down()
            && source_frames_per_output <= self.max_source_frames_per_output.next_up()
    }

    /// Largest request whose exact ratio rounds to `source_frames_per_output`.
    ///
    /// Returns `None` when the rate is outside this envelope or the supplied
    /// frame limits cannot represent its rounding basin.
    #[must_use]
    pub fn largest_request_at(
        self,
        source_frames_per_output: f64,
        max_source_frames: usize,
        max_output_frames: usize,
    ) -> Option<ElasticRequest> {
        if !self.contains_rate(source_frames_per_output) {
            return None;
        }
        let minimum = binary_midpoint(
            source_frames_per_output.next_down(),
            source_frames_per_output,
        )?;
        let maximum =
            binary_midpoint(source_frames_per_output, source_frames_per_output.next_up())?;
        largest_request_between(minimum, maximum, max_source_frames, max_output_frames)
            .filter(|request| self.contains(*request))
    }

    pub(crate) fn has_representable_request(
        self,
        max_source_frames: usize,
        max_output_frames: usize,
    ) -> bool {
        let accepted_minimum = self.min_source_frames_per_output.next_down();
        let accepted_maximum = self.max_source_frames_per_output.next_up();
        let Some(minimum) = binary_midpoint(accepted_minimum.next_down(), accepted_minimum) else {
            return false;
        };
        let Some(maximum) = binary_midpoint(accepted_maximum, accepted_maximum.next_up()) else {
            return false;
        };
        largest_request_between(minimum, maximum, max_source_frames, max_output_frames)
            .is_some_and(|request| self.contains(request))
    }
}

fn largest_request_between(
    minimum: (u128, u128),
    maximum: (u128, u128),
    max_source_frames: usize,
    max_output_frames: usize,
) -> Option<ElasticRequest> {
    let (_, denominator) = simplest_fraction(minimum, maximum, RATE_FRACTION_DEPTH)?;
    let max_output_frames = u128::try_from(max_output_frames).ok()?;
    if denominator > max_output_frames {
        return None;
    }
    let scaled_minimum = minimum.0.checked_mul(denominator)?;
    let source_frames =
        scaled_minimum / minimum.1 + u128::from(!scaled_minimum.is_multiple_of(minimum.1));
    let scaled_source = source_frames.checked_mul(maximum.1)?;
    let scaled_maximum = maximum.0.checked_mul(denominator)?;
    let max_source_frames = u128::try_from(max_source_frames).ok()?;
    if source_frames == 0 || source_frames > max_source_frames || scaled_source > scaled_maximum {
        return None;
    }
    let scale = (max_source_frames / source_frames).min(max_output_frames / denominator);
    let source_frames = usize::try_from(source_frames.checked_mul(scale)?).ok()?;
    let output_frames = usize::try_from(denominator.checked_mul(scale)?).ok()?;
    ElasticRequest::new(source_frames, output_frames).ok()
}

fn binary_midpoint(left: f64, right: f64) -> Option<(u128, u128)> {
    let left = binary_fraction(left)?;
    let right = binary_fraction(right)?;
    let denominator = left.1.max(right.1);
    let numerator = left
        .0
        .checked_mul(denominator / left.1)?
        .checked_add(right.0.checked_mul(denominator / right.1)?)?;
    Some((numerator, denominator.checked_mul(2)?))
}

fn binary_fraction(value: f64) -> Option<(u128, u128)> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let (mantissa, exponent, sign) = value.integer_decode();
    if sign <= 0 {
        return None;
    }
    let mantissa = u128::from(mantissa);
    if exponent >= 0 {
        Some((mantissa.checked_shl(u32::try_from(exponent).ok()?)?, 1))
    } else {
        Some((
            mantissa,
            1_u128.checked_shl(u32::from(exponent.unsigned_abs()))?,
        ))
    }
}

fn simplest_fraction(
    minimum: (u128, u128),
    maximum: (u128, u128),
    depth: u8,
) -> Option<(u128, u128)> {
    if depth == 0 {
        return None;
    }
    let whole = minimum.0 / minimum.1;
    let maximum_whole = maximum.0 / maximum.1;
    if whole < maximum_whole {
        return Some((whole.checked_add(1)?, 1));
    }
    let minimum_remainder = minimum.0 % minimum.1;
    if minimum_remainder == 0 {
        return Some((whole, 1));
    }
    let maximum_remainder = maximum.0 % maximum.1;
    let (numerator, denominator) = simplest_fraction(
        (maximum.1, maximum_remainder),
        (minimum.1, minimum_remainder),
        depth - 1,
    )?;
    Some((
        whole.checked_mul(numerator)?.checked_add(denominator)?,
        numerator,
    ))
}

impl TryFrom<RangeInclusive<f64>> for ElasticRateEnvelope {
    type Error = ElasticError;

    fn try_from(rates: RangeInclusive<f64>) -> Result<Self, Self::Error> {
        let (min_source_frames_per_output, max_source_frames_per_output) = rates.into_inner();
        if [min_source_frames_per_output, max_source_frames_per_output]
            .into_iter()
            .any(|rate| !rate.is_finite() || rate <= 0.0)
            || min_source_frames_per_output > max_source_frames_per_output
        {
            return Err(ElasticError::InvalidRateEnvelope {
                max: max_source_frames_per_output,
                min: min_source_frames_per_output,
            });
        }
        Ok(Self {
            max_source_frames_per_output,
            min_source_frames_per_output,
        })
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{ElasticError, ElasticRateEnvelope, ElasticRequest};

    fn envelope() -> ElasticRateEnvelope {
        ElasticRateEnvelope::try_from(2.0 / 3.0..=4.0 / 3.0)
            .expect("invariant: the declared window is finite, positive and ordered")
    }

    #[kithara::test]
    fn accepts_one_rounding_step_at_the_declared_rate_boundary() {
        let envelope = envelope();
        let minimum = envelope.min_source_frames_per_output();
        let maximum = envelope.max_source_frames_per_output();
        let one_step_below = minimum.next_down();
        let two_steps_below = one_step_below.next_down();
        let one_step_above = maximum.next_up();
        let two_steps_above = one_step_above.next_up();

        assert!(envelope.contains_rate(one_step_below));
        assert!(!envelope.contains_rate(two_steps_below));
        assert!(envelope.contains_rate(one_step_above));
        assert!(!envelope.contains_rate(two_steps_above));
    }

    #[kithara::test]
    fn rejects_windows_that_cannot_bound_a_rate() {
        for rates in [
            0.0..=1.0,
            f64::NAN..=1.0,
            1.0..=f64::INFINITY,
            4.0 / 3.0..=2.0 / 3.0,
        ] {
            assert!(matches!(
                ElasticRateEnvelope::try_from(rates),
                Err(ElasticError::InvalidRateEnvelope { .. })
            ));
        }
    }

    #[kithara::test]
    fn largest_request_preserves_exact_rates_inside_frame_limits() {
        let envelope = ElasticRateEnvelope::try_from(0.05..=4.0)
            .expect("invariant: practical rate envelope is valid");

        assert_eq!(
            envelope.largest_request_at(0.05, 8192, 64),
            ElasticRequest::new(3, 60).ok()
        );
        assert_eq!(
            envelope.largest_request_at(4.0, 8192, 64),
            ElasticRequest::new(256, 64).ok()
        );
        assert_eq!(envelope.largest_request_at(0.05, 8192, 19), None);
        assert_eq!(envelope.largest_request_at(0.049, 8192, 64), None);
    }
}
