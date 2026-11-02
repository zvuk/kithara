use std::{f32::consts::FRAC_1_SQRT_2, ops::DerefMut};

use biquad::{Biquad, Coefficients, DirectForm1, Type};

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
    frequency: f32,
    lowpass: Lr4,
    highpass: Lr4,
    allpass: Box<[Section]>,
}

/// Mono LR4 crossovers with later-frequency allpass compensation, computed in f32.
#[non_exhaustive]
pub struct Crossover {
    splits: Box<[Split]>,
}

impl Crossover {
    /// Allocates zeroed state. An unrepresentable design uses identity coefficients.
    #[must_use]
    pub fn new(frequencies: &[f32], sample_rate: f32) -> Self {
        Self {
            splits: frequencies
                .iter()
                .enumerate()
                .map(|(index, &frequency)| Split {
                    frequency,
                    lowpass: Lr4::new(coefficients(Type::LowPass, frequency, sample_rate)),
                    highpass: Lr4::new(coefficients(Type::HighPass, frequency, sample_rate)),
                    allpass: frequencies
                        .iter()
                        .skip(index.saturating_add(1))
                        .map(|&frequency| {
                            Section::new(coefficients(Type::AllPass, frequency, sample_rate))
                        })
                        .collect(),
                })
                .collect(),
        }
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

    /// Rebuilds the f32 designs at `sample_rate` and clears history without allocation.
    pub fn update_sample_rate(&mut self, sample_rate: f32) {
        let mut splits = self.splits.as_mut();
        while let Some((split, later)) = splits.split_first_mut() {
            split.lowpass = Lr4::new(coefficients(Type::LowPass, split.frequency, sample_rate));
            split.highpass = Lr4::new(coefficients(Type::HighPass, split.frequency, sample_rate));
            for (section, later) in split.allpass.iter_mut().zip(later.iter()) {
                *section = Section::new(coefficients(Type::AllPass, later.frequency, sample_rate));
            }
            splits = later;
        }
    }
}

fn coefficients(kind: Type<f32>, frequency: f32, sample_rate: f32) -> Coefficients<f32> {
    Coefficients::from_normalized_params(kind, 2.0 * frequency / sample_rate, FRAC_1_SQRT_2)
        .unwrap_or(Coefficients {
            a1: 0.0,
            a2: 0.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
        })
}
