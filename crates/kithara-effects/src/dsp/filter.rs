use kithara_dsp::filter::{Crossover, Hertz};

pub(crate) fn build_crossover(frequencies: &[f32], sample_rate: u32) -> (Crossover, usize) {
    assert!(
        frequencies
            .iter()
            .all(|frequency| { frequency.is_finite() && *frequency > 0.0 })
            && frequencies.windows(2).all(|pair| pair[0] < pair[1]),
        "EQ crossover frequencies must be finite, positive and strictly increasing"
    );
    let active_bands = active_band_count(frequencies, sample_rate);
    let nyquist = f64::from(sample_rate.max(1)) * 0.5;
    let frequencies: Vec<_> = frequencies
        .iter()
        .copied()
        .map(f64::from)
        .filter(|frequency| *frequency < nyquist)
        .map(hertz)
        .collect();
    let filters = Crossover::new(&frequencies, hertz(f64::from(sample_rate.max(1))))
        .unwrap_or_else(|error| panic!("validated EQ crossover design: {error}"));
    (filters, active_bands)
}

pub(crate) fn active_band_count(frequencies: &[f32], sample_rate: u32) -> usize {
    let nyquist = f64::from(sample_rate.max(1)) * 0.5;
    frequencies
        .iter()
        .filter(|frequency| f64::from(**frequency) < nyquist)
        .count()
        .saturating_add(1)
}

pub(crate) fn hertz(value: f64) -> Hertz<f64> {
    Hertz::from_hz(value).unwrap_or_else(|error| panic!("validated EQ hertz value: {error:?}"))
}
