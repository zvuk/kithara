use std::num::NonZeroUsize;

use crate::{ResamplerCapabilities, ResamplerError, ResamplerMode};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ResamplerProcess {
    pub input_frames: usize,
    pub output_frames: usize,
}

impl ResamplerProcess {
    #[must_use]
    pub const fn new(input_frames: usize, output_frames: usize) -> Self {
        Self {
            input_frames,
            output_frames,
        }
    }
}

/// Standalone planar PCM resampler.
pub trait Resampler: Send + 'static {
    fn capabilities(&self) -> ResamplerCapabilities;

    fn channels(&self) -> NonZeroUsize;

    /// Drain backend-owned tail frames into caller-owned planar output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] if the backend cannot flush its tail into the
    /// provided buffers.
    fn drain_into_buffer(&mut self, output: &mut [&mut [f32]]) -> Result<usize, ResamplerError> {
        let _ = output;
        Ok(0)
    }

    /// Process the final caller-supplied input block before draining backend tail.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] under the same conditions as
    /// [`Self::process_into_buffer`].
    fn flush_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        self.process_into_buffer(input, output)
    }

    fn input_frames_max(&self) -> usize;

    fn input_frames_next(&self) -> usize;

    fn mode(&self) -> ResamplerMode;

    fn output_delay(&self) -> usize {
        0
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize;

    fn output_frames_max(&self) -> usize;

    fn output_frames_next(&self) -> usize;

    /// Process one planar input block into caller-owned planar output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] when the buffers do not match the backend
    /// contract or processing fails.
    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError>;

    fn reset(&mut self);
}
