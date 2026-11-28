use kithara_dsp::filter::{Crossover, FilterError, Hertz};

pub(crate) fn build_crossover<const CHANNELS: usize>(
    frequencies: &[f32],
    sample_rate: u32,
) -> Result<(Crossover<CHANNELS>, usize), FilterError> {
    validate_frequencies(frequencies.iter().copied())?;
    let active_bands = active_band_count(frequencies, sample_rate);
    let nyquist = f64::from(sample_rate.max(1)) * 0.5;
    let frequencies: Vec<_> = frequencies
        .iter()
        .copied()
        .map(f64::from)
        .filter(|frequency| *frequency < nyquist)
        .map(hertz_checked)
        .collect::<Result<_, _>>()?;
    let filters = Crossover::new(&frequencies, hertz_checked(f64::from(sample_rate.max(1)))?)?;
    Ok((filters, active_bands))
}

pub(crate) fn validate_frequencies(
    frequencies: impl Iterator<Item = f32>,
) -> Result<(), FilterError> {
    let mut previous = 0.0;
    for frequency in frequencies {
        if !frequency.is_finite() || frequency <= previous {
            return Err(FilterError::Parameters);
        }
        previous = frequency;
    }
    Ok(())
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

fn hertz_checked(value: f64) -> Result<Hertz<f64>, FilterError> {
    Hertz::from_hz(value).map_err(|_| FilterError::Parameters)
}
