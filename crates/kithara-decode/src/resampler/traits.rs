use crate::resampler::ResamplerError;

/// Fixed-ratio planar PCM sample-rate converter.
pub trait Resampler: Send + 'static {
    /// Number of planar channels accepted by this backend.
    fn channels(&self) -> usize;

    /// Drain backend-owned tail frames into caller-owned planar output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] if the backend cannot flush its tail into the
    /// provided buffers.
    fn drain_into_buffer(&mut self, output: &mut [Vec<f32>]) -> Result<usize, ResamplerError> {
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
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResamplerError> {
        self.process_into_buffer(input, output)
    }

    /// Return the largest caller input block this backend is sized for.
    fn input_frames_max(&self) -> usize;

    /// Return the preferred caller input frame count for the next process call.
    fn input_frames_next(&self) -> usize;

    /// Backend group delay in output frames.
    fn output_delay(&self) -> usize {
        0
    }

    /// Return the output frame capacity for a caller-sized input block.
    fn output_frames_for_input(&self, input_frames: usize) -> usize;

    /// Return the backend's worst-case output block size.
    fn output_frames_max(&self) -> usize;

    /// Return the output frame capacity for the next preferred input block.
    fn output_frames_next(&self) -> usize;

    /// Process one planar input block into caller-owned planar output buffers.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerError`] when the buffers do not match the backend
    /// contract or processing fails. The tuple is `(input_accepted, output)`.
    fn process_into_buffer(
        &mut self,
        input: &[Vec<f32>],
        output: &mut [Vec<f32>],
    ) -> Result<(usize, usize), ResamplerError>;

    /// Fixed output/input frame ratio.
    fn ratio(&self) -> f64;

    /// Reset backend state to its initial fixed-ratio position.
    fn reset(&mut self);

    /// Source sample rate in Hz.
    fn source_rate(&self) -> u32;

    /// Target sample rate in Hz.
    fn target_rate(&self) -> u32;
}
