use std::ops::{DerefMut, Range};

use super::FilterError;
use crate::backend;

/// Fixed-coefficient low-pass: `y[n] = (1 - a)*x[n] + a*y[n - 1]`.
#[derive(Debug)]
pub struct OnePoleLowPass<const CHANNELS: usize> {
    coefficient: f32,
    state: [f32; CHANNELS],
}

impl<const CHANNELS: usize> OnePoleLowPass<CHANNELS> {
    /// Starts every channel at silence, with the feedback coefficient `a`.
    ///
    /// # Errors
    /// [`FilterError::Parameters`] unless `coefficient` is finite and in
    /// `0..1`; [`FilterError::Shape`] when `CHANNELS` is zero.
    pub fn new(coefficient: f32) -> Result<Self, FilterError> {
        if CHANNELS == 0 {
            return Err(FilterError::Shape);
        }
        Ok(Self {
            coefficient: checked_coefficient(coefficient)?,
            state: [0.0; CHANNELS],
        })
    }

    /// Changes the coefficient for the next frame, preserving every channel's state.
    ///
    /// # Errors
    /// [`FilterError::Parameters`] unless `coefficient` is finite and in
    /// `0..1`; refusal leaves the coefficient and channel histories unchanged.
    pub fn retune(&mut self, coefficient: f32) -> Result<(), FilterError> {
        self.coefficient = checked_coefficient(coefficient)?;
        Ok(())
    }

    /// Filters `range` in place, without allocating or reassociating audio time.
    ///
    /// # Errors
    /// [`FilterError::Shape`] when channel count or plane coverage differs;
    /// all samples and channel histories remain untouched on refusal.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), FilterError> {
        let planes: &mut [P; CHANNELS] = planes.try_into().map_err(|_| FilterError::Shape)?;
        if planes
            .iter()
            .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(FilterError::Shape);
        }
        let mut ranges = planes.each_mut().map(|plane| {
            let tail = plane.split_at_mut(range.start).1;
            tail.split_at_mut(range.len()).0
        });
        backend::one_pole_low_pass(self.coefficient, &mut self.state, &mut ranges);
        Ok(())
    }

    /// Clears the history of every channel, preserving the coefficient.
    pub fn reset(&mut self) {
        self.state.fill(0.0);
    }
}

fn checked_coefficient(coefficient: f32) -> Result<f32, FilterError> {
    if (0.0..1.0).contains(&coefficient) {
        Ok(coefficient)
    } else {
        Err(FilterError::Parameters)
    }
}
