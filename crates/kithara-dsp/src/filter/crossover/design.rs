use biquad::Hertz;
use fearless_simd::{Level, dispatch};

use crate::{backend::platform, filter::FilterError};

pub(super) fn validate(
    frequencies: impl Iterator<Item = Hertz<f64>>,
    sample_rate: Hertz<f64>,
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

pub(super) fn gain(frequency: Hertz<f64>, sample_rate: Hertz<f64>) -> Result<f64, FilterError> {
    validate(std::iter::once(frequency), sample_rate)?;
    let mut output = [0.0];
    dispatch!(Level::new(), simd => {
        platform::svf_gains(simd, &[frequency.hz()], sample_rate.hz(), &mut output);
    });
    if output[0].is_finite() && output[0] > 0.0 {
        Ok(output[0])
    } else {
        Err(FilterError::Parameters)
    }
}
