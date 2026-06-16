use thiserror::Error;

#[cfg(test)]
mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Raw detector output: beat / downbeat positions in seconds from track
/// start.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawBeats {
    pub(crate) beats: Vec<f32>,
    pub(crate) downbeats: Vec<f32>,
}

/// Failure of a beat detector backend.
#[derive(Debug, Error)]
pub(crate) enum BeatDetectError {
    /// Only the `beat-nn` factory constructs this; gated with it.
    #[cfg(feature = "beat-nn")]
    #[error("beat detector init failed: {reason}")]
    Init { reason: String },
    /// Detection can only fail when a detector backend runs (`beat-nn`) or a
    /// test scripts a failure; without either it is unconstructable.
    #[cfg(any(test, feature = "beat-nn"))]
    #[error("beat detection failed: {reason}")]
    Detect { reason: String },
}

/// Swappable beat/downbeat detector. Input is whole-track mono f32 at
/// 22 050 Hz by contract — downmix/resample is the caller's job.
#[cfg_attr(test, kithara::mock(api = [BeatDetectorMock]))]
pub(crate) trait BeatDetector: Send {
    /// # Errors
    /// [`BeatDetectError::Detect`] when the backend fails on this input.
    fn detect(&mut self, mono_22050: &[f32]) -> Result<RawBeats, BeatDetectError>;
}
