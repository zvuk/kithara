/// Error from a [`StretchBackend`]. Carried so the outer
/// `AudioEffect::process` (fixed at `-> Option<PcmChunk>`) maps a backend
/// failure to "drop this chunk + warn", never a panic.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum StretchBackendError {
    /// A `process` / `flush` call failed inside the library.
    #[error("stretch backend processing failed: {0}")]
    Process(String),
    /// An invalid ratio / pitch value was rejected by the library.
    #[error("invalid stretch parameter: {0}")]
    Param(String),
}

/// DSP-only time-stretch backend living behind the single
/// [`TimeStretchProcessor`](super::super::TimeStretchProcessor) slot.
///
/// Interleaved f32 in/out; the processor owns all `PcmChunk` / pool /
/// timeline plumbing so each library adapter stays tiny. Tempo
/// ([`set_ratio`](Self::set_ratio)) and pitch
/// ([`set_pitch`](Self::set_pitch)) are independent — that decoupling is
/// what makes keylock real. See `crates/kithara-audio/CONTEXT.md`.
pub trait StretchBackend: Send + 'static {
    /// Drain the buffered tail at end of stream. One-shot: once the tail is
    /// drained, further calls (until the next `process` or `reset`) must
    /// append nothing — the worker pulls `flush` in a loop until it yields
    /// an empty append (`drain_effects`).
    ///
    /// # Errors
    /// Returns [`StretchBackendError::Process`] if the library fails to drain.
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), StretchBackendError>;

    /// Upper bound on interleaved output samples a single `process` / `flush`
    /// can emit for `input_frames`, so the processor pre-reserves its output
    /// scratch once and stays alloc-free on the produce-core.
    fn max_output_samples(&self, input_frames: usize) -> usize;

    /// Push interleaved `input`; append whatever interleaved output is ready.
    ///
    /// Output frame count is governed by the ratio and may burst (latency
    /// fill, then catch-up); never assume `input.len() * ratio`.
    ///
    /// # Errors
    /// Returns [`StretchBackendError::Process`] if the library rejects the
    /// buffer (e.g. non-finite samples or an internal overflow).
    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), StretchBackendError>;

    /// Clear buffered state (after seek or source-spec change).
    fn reset(&mut self);

    /// Keylock pitch scale, independent of [`set_ratio`](Self::set_ratio).
    /// `1.0` keeps pitch locked (the tempo-mode default).
    ///
    /// # Errors
    /// Returns [`StretchBackendError::Param`] if `scale` is not a valid
    /// (finite, positive) pitch factor.
    fn set_pitch(&mut self, scale: f64) -> Result<(), StretchBackendError>;

    /// Time-stretch factor = `output_frames / input_frames` (>1 = slower /
    /// longer). The processor derives it from the shared playback speed.
    ///
    /// # Errors
    /// Returns [`StretchBackendError::Param`] if `stretch` is not a valid
    /// (finite, positive) ratio.
    fn set_ratio(&mut self, stretch: f64) -> Result<(), StretchBackendError>;
}
