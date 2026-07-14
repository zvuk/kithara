use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};

use super::{BeatDetectError, BeatDetector, RawBeats};

/// Beat detector selection.
#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub(crate) enum BeatDetectorKind {
    /// `kithara-beat` NN (`beat_this` port). Feature `beat-nn`.
    NnBeatThis,
}

impl BeatDetectorKind {
    /// Detectors compiled into this target/feature set, in selector order.
    pub(crate) const ALL: &'static [Self] = &[Self::NnBeatThis];

    /// First compiled-in detector, in selector order.
    pub(crate) fn first() -> Self {
        Self::ALL[0]
    }
}

impl Default for BeatDetectorKind {
    fn default() -> Self {
        Self::first()
    }
}

/// Construct the detector for `kind`.
///
/// # Errors
/// [`BeatDetectError::Init`] when the backend cannot load its models.
pub(crate) fn build_detector(
    kind: BeatDetectorKind,
) -> Result<Box<dyn BeatDetector>, BeatDetectError> {
    match kind {
        BeatDetectorKind::NnBeatThis => Ok(Box::new(NnDetector::new()?)),
    }
}

/// Adapter: `kithara-beat` NN behind the [`BeatDetector`] seam, built from
/// the embedded small-model bytes.
struct NnDetector {
    inner: BeatThis,
}

impl NnDetector {
    fn new() -> Result<Self, BeatDetectError> {
        let inner = BeatThis::try_from((MEL_MODEL_BYTES, BEAT_MODEL_BYTES)).map_err(|e| {
            BeatDetectError::Init {
                reason: e.to_string(),
            }
        })?;
        Ok(Self { inner })
    }
}

impl BeatDetector for NnDetector {
    fn detect(&mut self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        let raw = self
            .inner
            .analyze(mono_window)
            .map_err(|e| BeatDetectError::Detect {
                reason: e.to_string(),
            })?;
        Ok(RawBeats {
            beats: raw.beats,
            downbeats: raw.downbeats,
        })
    }
}
