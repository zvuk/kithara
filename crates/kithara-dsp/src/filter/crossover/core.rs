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

#[derive(Default)]
struct BranchState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BranchState {
    #[inline(always)]
    fn commit(&mut self, input: f32, output: f32, retired: bool) {
        if retired {
            *self = Self::default();
        } else {
            self.x2 = self.x1;
            self.x1 = input;
            self.y2 = self.y1;
            self.y1 = output;
        }
    }
}

struct Lr4Pair {
    coefficients: [[f32; 2]; 5],
    low: [BranchState; 2],
    high: [BranchState; 2],
}

impl Lr4Pair {
    fn new(low: Coefficients<f32>, high: Coefficients<f32>) -> Self {
        Self {
            coefficients: [
                [low.b0, high.b0],
                [low.b1, high.b1],
                [low.b2, high.b2],
                [low.a1, high.a1],
                [low.a2, high.a2],
            ],
            low: std::array::from_fn(|_| BranchState::default()),
            high: std::array::from_fn(|_| BranchState::default()),
        }
    }

    #[inline(always)]
    fn evaluate(&self, stage: usize, input: [f32; 2]) -> ([f32; 2], [bool; 2]) {
        let low = self.low.get(stage).expect("two LR4 stages");
        let high = self.high.get(stage).expect("two LR4 stages");
        let [[b00, b01], [b10, b11], [b20, b21], [a10, a11], [a20, a21]] = self.coefficients;
        let [left, right] = input;
        let l = b00 * left + b10 * low.x1 + b20 * low.x2 - a10 * low.y1 - a20 * low.y2;
        let r = b01 * right + b11 * high.x1 + b21 * high.x2 - a11 * high.y1 - a21 * high.y2;
        let rl = l.is_subnormal() && left.abs() < f32::MIN_POSITIVE;
        let rh = r.is_subnormal() && right.abs() < f32::MIN_POSITIVE;
        (
            [if rl { 0.0 } else { l }, if rh { 0.0 } else { r }],
            [rl, rh],
        )
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> [f32; 2] {
        let ([l1, h1], [rl1, rh1]) = self.evaluate(0, [input, input]);
        let ([low, high], [rl2, rh2]) = self.evaluate(1, [l1, h1]);
        self.low[0].commit(input, l1, rl1);
        self.low[1].commit(l1, low, rl2);
        self.high[0].commit(input, h1, rh1);
        self.high[1].commit(h1, high, rh2);
        [low, high]
    }

    fn reset(&mut self) {
        for state in self.low.iter_mut().chain(&mut self.high) {
            *state = BranchState::default();
        }
    }
}

struct Split {
    frequency: Hertz<f32>,
    branches: Lr4Pair,
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
                    branches: Lr4Pair::new(design.lowpass, design.highpass),
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
    pub fn split<P: DerefMut<Target = [f32]>>(&mut self, input: &[f32], bands: &mut [P]) -> usize {
        let Some(bands) = bands.get_mut(..self.splits.len().saturating_add(1)) else {
            return 0;
        };
        let count = bands
            .iter()
            .fold(input.len(), |count, band| count.min(band.len()));
        for (frame, &sample) in input.iter().take(count).enumerate() {
            for (band, value) in bands
                .iter_mut()
                .zip(filtered_bands(&mut self.splits, sample))
            {
                *band
                    .get_mut(frame)
                    .expect("frame belongs to the validated common prefix") = value;
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
        filtered_bands(&mut self.splits, sample)
            .zip(&self.gain_frame)
            .fold(0.0, |output, (value, gain)| value.mul_add(*gain, output))
    }

    /// Clears every section's history without allocation or coefficient changes.
    pub fn reset(&mut self) {
        for split in &mut self.splits {
            split.branches.reset();
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
            split.branches = Lr4Pair::new(prepared.lowpass, prepared.highpass);
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
fn filtered_bands(splits: &mut [Split], input: f32) -> impl Iterator<Item = f32> + '_ {
    let mut splits = splits.iter_mut();
    let mut high = Some(input);
    std::iter::from_fn(move || {
        let sample = high?;
        match splits.next() {
            Some(split) => {
                let [mut low, next_high] = split.branches.process(sample);
                high = Some(next_high);
                for section in &mut split.allpass {
                    low = section.run(low);
                }
                Some(low)
            }
            None => high.take(),
        }
    })
}
