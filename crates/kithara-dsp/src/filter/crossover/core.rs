use std::ops::{Deref, DerefMut};

use biquad::Hertz;

use super::{gain, validate};
use crate::{backend::crossover, filter::FilterError};

/// Planar LR4 crossovers with later-frequency allpass compensation on TPT sections.
#[non_exhaustive]
pub struct Crossover<const CHANNELS: usize = 1> {
    frequencies: Box<[Hertz<f64>]>,
    splits: Box<[crossover::Split<CHANNELS>]>,
    prepared: Box<[f64]>,
}

impl<const CHANNELS: usize> Crossover<CHANNELS> {
    /// Allocates zeroed state for ascending crossovers; an empty list is one identity band.
    ///
    /// # Errors
    /// Returns [FilterError::Parameters] for nonfinite/nonpositive rate or frequencies,
    /// crossovers not strictly increasing below Nyquist, or an unrepresentable design.
    pub fn new(frequencies: &[Hertz<f64>], sample_rate: Hertz<f64>) -> Result<Self, FilterError> {
        validate(frequencies.iter().copied(), sample_rate)?;
        let designs = frequencies
            .iter()
            .map(|&frequency| gain(frequency, sample_rate))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            frequencies: frequencies.into(),
            splits: designs
                .iter()
                .enumerate()
                .map(|(index, gain)| {
                    crossover::Split::new(
                        *gain,
                        designs.iter().copied().skip(index.saturating_add(1)),
                    )
                })
                .collect(),
            prepared: designs.into_boxed_slice(),
        })
    }

    /// Writes ascending bands of channel planes over the common input/output prefix.
    /// Missing bands is a no-op; extra bands and all unprocessed tails stay untouched.
    ///
    /// # Panics
    /// Panics if a plane changes length between dereferences during processing.
    pub fn split<I: Deref<Target = [f32]>, P: DerefMut<Target = [f32]>>(
        &mut self,
        input: &[I; CHANNELS],
        bands: &mut [[P; CHANNELS]],
    ) -> usize {
        let Some(bands) = bands.get_mut(..self.splits.len().saturating_add(1)) else {
            return 0;
        };
        let frames = input.iter().map(|plane| plane.len()).min().unwrap_or(0);
        let frames = bands
            .iter()
            .flatten()
            .fold(frames, |count, plane| count.min(plane.len()));
        crossover::process(&mut self.splits, input, bands, frames);
        frames
    }

    /// Clears every section's history without allocation or coefficient changes.
    pub fn reset(&mut self) {
        for split in &mut self.splits {
            split.reset();
        }
    }

    /// Atomically retunes every section, clearing history without allocation.
    ///
    /// # Errors
    /// Returns [FilterError::Parameters] if the rate cannot represent every crossover.
    /// A refusal leaves the active design and history unchanged.
    pub fn update_sample_rate(&mut self, sample_rate: Hertz<f64>) -> Result<(), FilterError> {
        validate(self.frequencies.iter().copied(), sample_rate)?;
        for (frequency, prepared) in self.frequencies.iter().zip(self.prepared.iter_mut()) {
            *prepared = gain(*frequency, sample_rate)?;
        }
        for (index, (split, prepared)) in self.splits.iter_mut().zip(&self.prepared).enumerate() {
            split.retune(
                *prepared,
                self.prepared.iter().copied().skip(index.saturating_add(1)),
            );
        }
        Ok(())
    }
}
