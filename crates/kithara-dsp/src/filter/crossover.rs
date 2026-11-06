use std::{
    f32::consts::FRAC_1_SQRT_2,
    ops::{Deref, DerefMut},
};

use biquad::{Coefficients, Hertz, Type};

use super::FilterError;

#[cfg(test)]
mod tests;

struct Df1State {
    y1: f32,
    y2: f32,
    x1: f32,
    x2: f32,
}

impl Df1State {
    const fn new() -> Self {
        Self {
            y1: 0.0,
            y2: 0.0,
            x1: 0.0,
            x2: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, input: f32, coefficients: &Coefficients<f32>) -> f32 {
        let output =
            coefficients.b0 * input + coefficients.b1 * self.x1 + coefficients.b2 * self.x2
                - coefficients.a1 * self.y1
                - coefficients.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.reset();
            return 0.0;
        }
        output
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

struct Section {
    state: Df1State,
    coefficients: Coefficients<f32>,
}

impl Section {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self {
            state: Df1State::new(),
            coefficients,
        }
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        self.state.run(input, &self.coefficients)
    }
}

struct Lr4 {
    coefficients: Coefficients<f32>,
    first: Df1State,
    second: Df1State,
}

impl Lr4 {
    const fn new(coefficients: Coefficients<f32>) -> Self {
        Self {
            coefficients,
            first: Df1State::new(),
            second: Df1State::new(),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.run(
            self.first.run(input, &self.coefficients),
            &self.coefficients,
        )
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
            frame: vec![0.0; frequencies.len()].into_boxed_slice(),
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
        let (high_band, low_bands) = bands
            .split_last_mut()
            .expect("one high band after the crossovers");
        for (frame, &sample) in input.iter().take(count).enumerate() {
            let high = self.filter_sample(sample);
            for (band, value) in low_bands.iter_mut().zip(self.frame.iter()) {
                *band
                    .get_mut(frame)
                    .expect("frame belongs to the validated common prefix") = *value;
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
        let (high_gain, low_gains) = gains
            .split_last()
            .expect("one high gain after the crossovers");
        for (frame, sample) in samples.iter_mut().take(count).enumerate() {
            let high = self.filtered_high(*sample);
            let low = self
                .frame
                .iter()
                .zip(low_gains)
                .fold(0.0, |output, (value, gain)| {
                    value.mul_add(
                        *gain
                            .get(frame)
                            .expect("frame belongs to the validated common prefix"),
                        output,
                    )
                });
            *sample = high.mul_add(
                *high_gain
                    .get(frame)
                    .expect("frame belongs to the validated common prefix"),
                low,
            );
        }
        count
    }

    #[inline(never)]
    fn filtered_high(&mut self, input: f32) -> f32 {
        self.filter_sample(input)
    }

    #[inline(always)]
    fn filter_sample(&mut self, mut high: f32) -> f32 {
        let lower = self.frame.as_mut();
        for ((lowpass, highpass), low) in self
            .lowpass
            .iter_mut()
            .zip(self.highpass.iter_mut())
            .zip(lower.iter_mut())
        {
            *low = lowpass.process(high);
            high = highpass.process(high);
        }
        let count = lower.len();
        let mut allpass = self.allpass.as_mut();
        for (size, low) in (1..count).rev().zip(lower.iter_mut()) {
            let (sections, remaining) = allpass.split_at_mut(size);
            let mut value = *low;
            for section in sections {
                value = section.run(value);
            }
            *low = value;
            allpass = remaining;
        }
        high
    }

    /// Clears every section's history without allocation or coefficient changes.
    pub fn reset(&mut self) {
        for filter in self.lowpass.iter_mut().chain(self.highpass.iter_mut()) {
            filter.first.reset();
            filter.second.reset();
        }
        for section in &mut self.allpass {
            section.state.reset();
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
