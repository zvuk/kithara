use std::f32::consts::FRAC_1_SQRT_2;

use biquad::{Coefficients, Hertz, Type};

use crate::filter::FilterError;

pub(super) fn validate(
    frequencies: impl Iterator<Item = Hertz<f32>>,
    sample_rate: Hertz<f32>,
) -> Result<(), FilterError> {
    let rate = sample_rate.hz();
    if !rate.is_finite() || rate <= 0.0 {
        return Err(FilterError::Parameters);
    }
    let mut previous = 0.0;
    for frequency in frequencies {
        let frequency = frequency.hz();
        if !frequency.is_finite() || frequency <= previous || frequency >= rate * 0.5 {
            return Err(FilterError::Parameters);
        }
        previous = frequency;
    }
    Ok(())
}

pub(super) fn coefficients(
    kind: Type<f32>,
    frequency: Hertz<f32>,
    sample_rate: Hertz<f32>,
) -> Result<Coefficients<f32>, FilterError> {
    let coefficients = Coefficients::from_normalized_params(
        kind,
        2.0 * frequency.hz() / sample_rate.hz(),
        FRAC_1_SQRT_2,
    )
    .map_err(|_| FilterError::Parameters)?;
    let Coefficients { a1, a2, b0, b1, b2 } = coefficients;
    if [a1, a2, b0, b1, b2].iter().any(|value| !value.is_finite()) {
        return Err(FilterError::Parameters);
    }
    Ok(coefficients)
}
