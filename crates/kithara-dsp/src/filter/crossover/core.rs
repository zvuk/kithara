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
    frequencies: Box<[Hertz<f32>]>,
    lowpass: Box<[Lr4]>,
    highpass: Box<[Lr4]>,
    allpass: Box<[Section]>,
    frame: Box<[f32]>,
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
                .flat_map(|start| designs.iter().skip(start))
                .map(|later| Section::new(later.allpass))
                .collect(),
            frame: vec![0.0; frequencies.len()].into_boxed_slice(),
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
        let Some(bands) = bands.get_mut(..self.frequencies.len().saturating_add(1)) else {
            return 0;
        };
        let count = bands
            .iter()
            .fold(input.len(), |count, band| count.min(band.len()));
        let (high_band, lower) = bands
            .split_last_mut()
            .expect("validated bands include the final high output");
        for (frame, &sample) in input.iter().take(count).enumerate() {
            let high = self.filter_sample(sample);
            for (band, &value) in lower.iter_mut().zip(self.frame.iter()) {
                *band
                    .get_mut(frame)
                    .expect("frame belongs to the validated common prefix") = value;
            }
            *high_band
                .get_mut(frame)
                .expect("frame belongs to the validated common prefix") = high;
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
        let Some(gains) = gains.get(..self.frequencies.len().saturating_add(1)) else {
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
        let high = self.filter_sample(sample);
        let (high_gain, lower) = self
            .gain_frame
            .split_last()
            .expect("construction reserves a gain for the final high band");
        let mut output = 0.0;
        for (&value, &gain) in self.frame.iter().zip(lower) {
            output = value.mul_add(gain, output);
        }
        high.mul_add(*high_gain, output)
    }

    #[inline(always)]
    fn filter_sample(&mut self, mut high: f32) -> f32 {
        for ((lowpass, highpass), low) in self
            .lowpass
            .iter_mut()
            .zip(self.highpass.iter_mut())
            .zip(self.frame.iter_mut())
        {
            *low = lowpass.process(high);
            high = highpass.process(high);
        }
        let mut remaining = self.allpass.as_mut();
        for (count, low) in (0..self.frame.len()).rev().zip(self.frame.iter_mut()) {
            let (sections, tail) = remaining.split_at_mut(count);
            remaining = tail;
            for section in sections {
                *low = section.run(*low);
            }
        }
        high
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
        for (filter, prepared) in self.lowpass.iter_mut().zip(self.prepared.iter()) {
            *filter = Lr4::new(prepared.lowpass);
        }
        for (filter, prepared) in self.highpass.iter_mut().zip(self.prepared.iter()) {
            *filter = Lr4::new(prepared.highpass);
        }
        let prepared = &self.prepared;
        let later = (1..prepared.len()).flat_map(|start| prepared.iter().skip(start));
        for (section, design) in self.allpass.iter_mut().zip(later) {
            *section = Section::new(design.allpass);
        }
        Some(rate)
    }
}
