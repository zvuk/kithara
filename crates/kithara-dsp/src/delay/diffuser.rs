use std::ops::{DerefMut, Range};

use num_traits::ToPrimitive;

use super::{DelayError, DelayLine, DiffuserError, Tap};
use crate::backend::platform;

const SCRATCH_FRAMES: usize = 128;

/// Delay diffusion with `d = read; y = d - x; write = x + g * d`.
///
/// Channels have independent history and share the same integer delay and
/// feedback. This recurrence does not have a unity-magnitude response.
#[derive(Debug)]
pub struct DelayDiffuser<const CHANNELS: usize> {
    line: DelayLine,
    tap: Tap,
    feedback: f32,
}

impl<const CHANNELS: usize> DelayDiffuser<CHANNELS> {
    /// Consumes prepared history, using its capacity as the delay in frames.
    ///
    /// # Errors
    /// [`DiffuserError::Feedback`] unless `feedback` is finite and inside
    /// `(-1, 1)`; [`DelayError::Shape`] if the history's channel count differs
    /// from `CHANNELS`.
    pub fn new(line: DelayLine, feedback: f32) -> Result<Self, DiffuserError> {
        if !feedback.is_finite() || feedback.abs() >= 1.0 {
            return Err(DiffuserError::Feedback);
        }
        if line.channels().get() != CHANNELS {
            return Err(DelayError::Shape.into());
        }
        let frames = line
            .capacity()
            .get()
            .to_f64()
            .ok_or(DelayError::OutOfRange)?;
        let tap = line.tap(frames)?;
        Ok(Self {
            line,
            tap,
            feedback,
        })
    }

    /// Replaces `range` of every plane with its diffused signal, without allocation.
    ///
    /// Reads precede writes in spans no longer than the delay. Backend kernels
    /// preserve the operation order across block partitions.
    ///
    /// # Errors
    /// [`DelayError::Shape`] when the channel count or plane coverage does not
    /// match; all samples and history remain untouched on refusal.
    pub fn process<P: DerefMut<Target = [f32]>>(
        &mut self,
        planes: &mut [P],
        range: Range<usize>,
    ) -> Result<(), DiffuserError> {
        if planes.len() != CHANNELS
            || planes
                .iter()
                .any(|plane| plane.get(range.clone()).is_none())
        {
            return Err(DelayError::Shape.into());
        }
        let mut scratch = [[0.0; SCRATCH_FRAMES]; CHANNELS];
        let mut writes = [[0.0; SCRATCH_FRAMES]; CHANNELS];
        let span = self.tap.whole().min(SCRATCH_FRAMES);
        let mut start = range.start;
        while start < range.end {
            let end = start.saturating_add(span).min(range.end);
            let len = end.wrapping_sub(start);
            for (channel, ((plane, history), writes)) in planes
                .iter_mut()
                .zip(&mut scratch)
                .zip(&mut writes)
                .enumerate()
            {
                let history = history.get_mut(..len).ok_or(DelayError::Shape)?;
                self.line.read(channel, self.tap, history)?;
                let samples = plane.get_mut(start..end).ok_or(DelayError::Shape)?;
                let writes = writes.get_mut(..len).ok_or(DelayError::Shape)?;
                platform::delay_diffuse(samples, history, self.feedback, writes);
            }
            self.line
                .write(&writes.each_ref().map(|plane| plane.as_slice()), 0..len)?;
            start = end;
        }
        Ok(())
    }

    /// Clears every channel's history without changing its delay or feedback.
    pub fn reset(&mut self) {
        self.line.reset();
    }
}
