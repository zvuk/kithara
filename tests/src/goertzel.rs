#[must_use]
pub fn goertzel_magnitude(samples: &[f32], freq_hz: f64, sample_rate: u32) -> f64 {
    let omega = 2.0 * std::f64::consts::PI * freq_hz / f64::from(sample_rate);
    let coeff = 2.0 * omega.cos();
    let mut previous = 0.0f64;
    let mut before_previous = 0.0f64;
    for sample in samples {
        let current = coeff * previous - before_previous + f64::from(*sample);
        before_previous = previous;
        previous = current;
    }
    let real = previous - before_previous * omega.cos();
    let imag = before_previous * omega.sin();
    (real * real + imag * imag).sqrt()
}
