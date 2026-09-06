use kithara_bufpool::PoolError;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BeatMark {
    pub(crate) at: f32,
    pub(crate) confidence: f32,
}

#[cfg(test)]
impl BeatMark {
    pub(crate) const fn at(at: f32) -> Self {
        Self {
            at,
            confidence: 0.9,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawBeats {
    pub(crate) beats: Vec<BeatMark>,
    pub(crate) downbeats: Vec<BeatMark>,
}

#[derive(Debug, Error)]
pub(crate) enum BeatDetectError {
    #[error("beat analysis buffer allocation failed: {0}")]
    Buffer(#[from] PoolError),
    #[error("beat analysis resampler failed: {reason}")]
    Resample { reason: String },
    #[cfg(feature = "beat-nn")]
    #[error("beat detector init failed: {reason}")]
    Init { reason: String },
    #[cfg(any(test, feature = "beat-nn"))]
    #[error("beat detection failed: {reason}")]
    Detect { reason: String },
}

#[cfg_attr(test, kithara_test_macros::mock(api = [BeatDetectorMock]))]
pub(crate) trait BeatDetector: Send + Sync {
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError>;
}
