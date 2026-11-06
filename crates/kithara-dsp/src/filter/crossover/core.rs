use std::ops::{Deref, DerefMut};

use biquad::{Biquad, Coefficients, DirectForm1, Hertz, Type};

use super::{coefficients, validate};
use crate::filter::FilterError;

// Keep each band's working window to 256 bytes; input length remains unrestricted.
const BLOCK_FRAMES: usize = 64;

#[derive(Clone, Copy)]
struct Section(DirectForm1<f32>);

impl Section {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self(DirectForm1::new(coefficients))
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        let output = self.0.run(input);
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            return self.retire();
        }
        output
    }

    #[cold]
    #[inline(never)]
    fn retire(&mut self) -> f32 {
        self.0.reset_state();
        0.0
    }

    #[inline(always)]
    fn process_block(&mut self, samples: &mut [f32]) {
        let mut section = *self;
        for sample in samples {
            *sample = section.run(*sample);
        }
        *self = section;
    }
}

#[derive(Clone, Copy)]
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

    #[inline(always)]
    fn process_block(&mut self, samples: &mut [f32]) {
        let mut cascade = *self;
        for sample in samples {
            *sample = cascade.process(*sample);
        }
        *self = cascade;
    }
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
    lowpass: Vec<Lr4>,
    highpass: Vec<Lr4>,
    band_blocks: Box<[[f32; BLOCK_FRAMES]]>,
    gain_blocks: Box<[[f32; BLOCK_FRAMES]]>,
    allpass: Vec<Section>,
    frequencies: Box<[Hertz<f32>]>,
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
            frequencies: frequencies.into(),
            lowpass: designs
                .iter()
                .map(|design| Lr4::new(design.lowpass))
                .collect(),
            highpass: designs
                .iter()
                .map(|design| Lr4::new(design.highpass))
                .collect(),
            allpass: (1..designs.len())
                .flat_map(|start| {
                    designs
                        .iter()
                        .skip(start)
                        .map(|design| Section::new(design.allpass))
                })
                .collect(),
            band_blocks: vec![[0.0; BLOCK_FRAMES]; frequencies.len().saturating_add(1)]
                .into_boxed_slice(),
            gain_blocks: vec![[0.0; BLOCK_FRAMES]; frequencies.len().saturating_add(1)]
                .into_boxed_slice(),
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
        let Some(bands) = bands.get_mut(..self.lowpass.len().saturating_add(1)) else {
            return 0;
        };
        let count = bands
            .iter()
            .fold(input.len(), |count, band| count.min(band.len()));
        for (index, input) in input
            .get(..count)
            .expect("validated input prefix")
            .chunks(BLOCK_FRAMES)
            .enumerate()
        {
            let start = index
                .checked_mul(BLOCK_FRAMES)
                .expect("chunk starts within the input prefix");
            let end = start
                .checked_add(input.len())
                .expect("chunk ends within the input prefix");
            self.filter_block(input);
            for (output, band) in bands.iter_mut().zip(self.band_blocks.iter()) {
                output
                    .get_mut(start..end)
                    .expect("validated common prefix remains available")
                    .copy_from_slice(
                        band.get(..input.len())
                            .expect("chunk fits the filter window"),
                    );
            }
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
        let Some(gains) = gains.get(..self.lowpass.len().saturating_add(1)) else {
            return 0;
        };
        let count = gains
            .iter()
            .fold(samples.len(), |n, gain| n.min(gain.len()));
        for (index, samples) in samples
            .get_mut(..count)
            .expect("validated output prefix")
            .chunks_mut(BLOCK_FRAMES)
            .enumerate()
        {
            let start = index
                .checked_mul(BLOCK_FRAMES)
                .expect("chunk starts within the output prefix");
            let end = start
                .checked_add(samples.len())
                .expect("chunk ends within the output prefix");
            for (row, gain) in self.gain_blocks.iter_mut().zip(gains) {
                row.get_mut(..samples.len())
                    .expect("chunk fits the gain window")
                    .copy_from_slice(
                        gain.get(start..end)
                            .expect("validated gain prefix remains available"),
                    );
            }
            self.weighted_block(samples);
        }
        count
    }

    #[inline(never)]
    fn weighted_block(&mut self, samples: &mut [f32]) {
        self.filter_block(samples);
        samples.fill(0.0);
        for (band, gain) in self.band_blocks.iter().zip(self.gain_blocks.iter()) {
            crate::mul_add(
                band.get(..samples.len())
                    .expect("chunk fits the filter window"),
                gain.get(..samples.len())
                    .expect("chunk fits the gain window"),
                samples,
            );
        }
    }

    #[inline(never)]
    fn filter_block(&mut self, input: &[f32]) {
        let (high, low) = self
            .band_blocks
            .split_last_mut()
            .expect("one high band after the crossovers");
        let high = high
            .get_mut(..input.len())
            .expect("chunk fits the filter window");
        high.copy_from_slice(input);
        for ((lowpass, highpass), low) in self
            .lowpass
            .iter_mut()
            .zip(self.highpass.iter_mut())
            .zip(low.iter_mut())
        {
            let low = low
                .get_mut(..input.len())
                .expect("chunk fits the filter window");
            low.copy_from_slice(high);
            lowpass.process_block(low);
            highpass.process_block(high);
        }
        let mut allpass = self.allpass.as_mut_slice();
        for (size, low) in (1..low.len()).rev().zip(low.iter_mut()) {
            let (sections, rest) = allpass.split_at_mut(size);
            allpass = rest;
            let low = low
                .get_mut(..input.len())
                .expect("chunk fits the filter window");
            for section in sections {
                section.process_block(low);
            }
        }
    }

    /// Clears every section's history without allocation or coefficient changes.
    pub fn reset(&mut self) {
        for filter in self.lowpass.iter_mut().chain(self.highpass.iter_mut()) {
            filter.first.0.reset_state();
            filter.second.0.reset_state();
        }
        for section in &mut self.allpass {
            section.0.reset_state();
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
        validate(self.frequencies.iter().copied(), sample_rate)?;
        for (&frequency, prepared) in self.frequencies.iter().zip(self.prepared.iter_mut()) {
            *prepared = Design::new(frequency, sample_rate)?;
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
        for ((lowpass, highpass), prepared) in self
            .lowpass
            .iter_mut()
            .zip(self.highpass.iter_mut())
            .zip(&self.prepared)
        {
            *lowpass = Lr4::new(prepared.lowpass);
            *highpass = Lr4::new(prepared.highpass);
        }
        let mut allpass = self.allpass.iter_mut();
        for index in 0..self.prepared.len() {
            let start = index.saturating_add(1);
            for (section, later) in allpass
                .by_ref()
                .take(self.prepared.len().saturating_sub(start))
                .zip(self.prepared.iter().skip(start))
            {
                *section = Section::new(later.allpass);
            }
        }
        Some(rate)
    }
}
