use std::{f32::consts::FRAC_1_SQRT_2, ops::DerefMut};

use biquad::{Biquad, Coefficients, DirectForm1, Hertz, Type};

use super::FilterError;

#[cfg(test)]
#[path = "crossover_tests.rs"]
mod tests;

struct Section(DirectForm1<f32>);

impl Section {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self(DirectForm1::new(coefficients))
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        let output = self.0.run(input);
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.0.reset_state();
            return 0.0;
        }
        output
    }
}

struct Lr4 {
    first: Section,
    second: Section,
}

impl Lr4 {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self {
            first: Section::new(coefficients),
            second: Section::new(coefficients),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.run(self.first.run(input))
    }
}

struct Split {
    frequency: Hertz<f32>,
    lowpass: Lr4,
    highpass: Lr4,
    allpass: Box<[Section]>,
    prepared: Design,
}

#[derive(Clone, Copy)]
struct Design {
    lowpass: Coefficients<f32>,
    highpass: Coefficients<f32>,
    allpass: Coefficients<f32>,
}

impl Design {
    fn new(frequency: Hertz<f32>, sample_rate: Hertz<f32>) -> Result<Self, FilterError> {
        Ok(Self {
            lowpass: coefficients(Type::LowPass, frequency, sample_rate)?,
            highpass: coefficients(Type::HighPass, frequency, sample_rate)?,
            allpass: coefficients(Type::AllPass, frequency, sample_rate)?,
        })
    }
}

/// Mono LR4 crossovers with later-frequency allpass compensation, computed in f32.
#[non_exhaustive]
pub struct Crossover {
    splits: Box<[Split]>,
    prepared_rate: Option<Hertz<f32>>,
}

impl Crossover {
    /// Allocates zeroed state for ascending crossovers; an empty list is one identity band.
    ///
    /// # Errors
    /// Returns [`FilterError::Parameters`] for nonfinite/nonpositive rate or frequencies,
    /// crossovers not strictly increasing below Nyquist, or an unrepresentable f32 design.
    pub fn new(frequencies: &[Hertz<f32>], sample_rate: Hertz<f32>) -> Result<Self, FilterError> {
        validate(frequencies.iter().copied(), sample_rate)?;
        let designs = frequencies
            .iter()
            .map(|&frequency| Design::new(frequency, sample_rate))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            splits: frequencies
                .iter()
                .zip(&designs)
                .enumerate()
                .map(|(index, (&frequency, design))| Split {
                    frequency,
                    lowpass: Lr4::new(design.lowpass),
                    highpass: Lr4::new(design.highpass),
                    allpass: designs
                        .iter()
                        .skip(index.saturating_add(1))
                        .map(|later| Section::new(later.allpass))
                        .collect(),
                    prepared: *design,
                })
                .collect(),
            prepared_rate: None,
        })
    }

    /// Writes ascending band planes over their common input prefix, returning its length.
    /// Fewer than `frequencies.len()` + 1 planes is a no-op; extra planes and tails stay untouched.
    pub fn split<P: DerefMut<Target = [f32]>>(&mut self, input: &[f32], bands: &mut [P]) -> usize {
        let Some(bands) = bands.get_mut(..self.splits.len().saturating_add(1)) else {
            return 0;
        };
        let count = bands
            .iter()
            .fold(input.len(), |count, band| count.min(band.len()));
        let Some((high, lower)) = bands.split_last_mut() else {
            return 0;
        };
        for (output, &sample) in high.iter_mut().zip(input).take(count) {
            *output = sample;
        }
        for (split, low) in self.splits.iter_mut().zip(lower) {
            for (low, high) in low.iter_mut().zip(high.iter_mut()).take(count) {
                *low = split.lowpass.process(*high);
                *high = split.highpass.process(*high);
                for section in &mut split.allpass {
                    *low = section.run(*low);
                }
            }
        }
        count
    }

    /// Clears every section's history without allocation or coefficient changes.
    pub fn reset(&mut self) {
        for split in &mut self.splits {
            split.lowpass.first.0.reset_state();
            split.lowpass.second.0.reset_state();
            split.highpass.first.0.reset_state();
            split.highpass.second.0.reset_state();
            for section in &mut split.allpass {
                section.0.reset_state();
            }
        }
    }

    /// Prepares a retune without changing active coefficients or history, or allocating.
    /// Owners of several crossovers can prepare them all before applying any retune.
    ///
    /// # Errors
    /// Returns [`FilterError::Parameters`] if the rate cannot represent every crossover.
    /// A rejected preparation invalidates prior preparation and leaves active state unchanged.
    pub fn prepare_sample_rate(&mut self, sample_rate: Hertz<f32>) -> Result<(), FilterError> {
        self.prepared_rate = None;
        validate(self.splits.iter().map(|split| split.frequency), sample_rate)?;
        for split in &mut self.splits {
            split.prepared = Design::new(split.frequency, sample_rate)?;
        }
        self.prepared_rate = Some(sample_rate);
        Ok(())
    }

    /// Applies a prepared retune, or prepares it first, then clears history without allocation.
    ///
    /// # Errors
    /// Returns [`FilterError::Parameters`] if preparation fails; active state remains unchanged.
    pub fn update_sample_rate(&mut self, sample_rate: Hertz<f32>) -> Result<(), FilterError> {
        if self.prepared_rate != Some(sample_rate) {
            self.prepare_sample_rate(sample_rate)?;
        }
        let _ = self.apply_prepared_sample_rate();
        Ok(())
    }

    /// Consumes the last successful preparation, clearing history without another factory call.
    /// Returns its rate, or `None` without mutation when no preparation remains.
    pub fn apply_prepared_sample_rate(&mut self) -> Option<Hertz<f32>> {
        let rate = self.prepared_rate.take()?;
        let mut splits = self.splits.as_mut();
        while let Some((split, later)) = splits.split_first_mut() {
            split.lowpass = Lr4::new(split.prepared.lowpass);
            split.highpass = Lr4::new(split.prepared.highpass);
            for (section, later) in split.allpass.iter_mut().zip(later.iter()) {
                *section = Section::new(later.prepared.allpass);
            }
            splits = later;
        }
        Some(rate)
    }
}

fn validate(
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

fn coefficients(
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
