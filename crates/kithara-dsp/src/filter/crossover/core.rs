use std::ops::{Deref, DerefMut};

use biquad::{Biquad, Coefficients, DirectForm1, Hertz, Type};

use super::{coefficients, validate};
use crate::filter::FilterError;

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
    gain_frame: Box<[f32]>,
    prepared: Box<[Design]>,
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
                })
                .collect(),
            gain_frame: vec![0.0; frequencies.len().saturating_add(1)].into_boxed_slice(),
            prepared: designs.into_boxed_slice(),
            prepared_rate: None,
        })
    }

    /// Writes ascending band planes over their common input prefix, returning its length.
    /// Fewer than `frequencies.len()` + 1 planes is a no-op; extra planes and tails stay untouched.
    ///
    /// # Panics
    /// Panics if a plane changes length between dereferences during processing.
    #[inline(always)]
    pub fn split<P: DerefMut<Target = [f32]>>(&mut self, input: &[f32], bands: &mut [P]) -> usize {
        let Some(bands) = bands.get_mut(..self.splits.len().saturating_add(1)) else {
            return 0;
        };
        let count = bands
            .iter()
            .fold(input.len(), |count, band| count.min(band.len()));
        for (frame, &sample) in input.iter().take(count).enumerate() {
            filtered_bands(&mut self.splits, sample, bands, |band, value| {
                *band
                    .get_mut(frame)
                    .expect("frame belongs to the validated common prefix") = value;
            });
        }
        count
    }

    /// Sums ascending bands in place with fused per-frame gains.
    /// Missing required gain planes is a no-op; short planes bound the processed prefix.
    /// Extra gain planes and the remaining samples stay untouched.
    ///
    /// # Panics
    /// Panics if a gain plane changes length between dereferences during processing.
    pub fn sum<P: Deref<Target = [f32]>>(&mut self, samples: &mut [f32], gains: &[P]) -> usize {
        let Some(gains) = gains.get(..self.splits.len().saturating_add(1)) else {
            return 0;
        };
        let count = gains
            .iter()
            .fold(samples.len(), |n, gain| n.min(gain.len()));
        for (frame, sample) in samples.iter_mut().take(count).enumerate() {
            for (value, gain) in self.gain_frame.iter_mut().zip(gains) {
                *value = *gain
                    .get(frame)
                    .expect("frame belongs to the validated common prefix");
            }
            *sample = self.weighted_sample(*sample);
        }
        count
    }

    #[inline(never)]
    fn weighted_sample(&mut self, sample: f32) -> f32 {
        let mut output = 0.0;
        filtered_bands(
            &mut self.splits,
            sample,
            &mut self.gain_frame,
            |gain, value| {
                output = value.mul_add(*gain, output);
            },
        );
        output
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
        for (split, prepared) in self.splits.iter().zip(self.prepared.iter_mut()) {
            *prepared = Design::new(split.frequency, sample_rate)?;
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
        for (index, (split, prepared)) in self.splits.iter_mut().zip(&self.prepared).enumerate() {
            split.lowpass = Lr4::new(prepared.lowpass);
            split.highpass = Lr4::new(prepared.highpass);
            for (section, later) in split
                .allpass
                .iter_mut()
                .zip(self.prepared.iter().skip(index.saturating_add(1)))
            {
                *section = Section::new(later.allpass);
            }
        }
        Some(rate)
    }
}

#[inline(always)]
fn filtered_bands<T>(
    splits: &mut [Split],
    mut high: f32,
    outputs: &mut [T],
    mut consume: impl FnMut(&mut T, f32),
) {
    let (high_output, lower) = outputs
        .split_last_mut()
        .expect("validated bands include the final high output");
    for (split, output) in splits.iter_mut().zip(lower) {
        let mut low = split.lowpass.process(high);
        high = split.highpass.process(high);
        for section in &mut split.allpass {
            low = section.run(low);
        }
        consume(output, low);
    }
    consume(high_output, high);
}
