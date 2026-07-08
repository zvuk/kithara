#[cfg(feature = "beat-nn")]
use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};

use super::{
    detector::{BeatDetectError, BeatDetector, RawBeats},
    detector_kind::BeatDetectorKind,
};

/// Construct the detector for `kind`.
///
/// # Errors
/// [`BeatDetectError::Init`] when the backend cannot load its models.
pub(crate) fn build_detector(
    kind: BeatDetectorKind,
) -> Result<Box<dyn BeatDetector>, BeatDetectError> {
    match kind {
        #[cfg(feature = "beat-nn")]
        BeatDetectorKind::NnBeatThis => Ok(Box::new(NnDetector::new()?)),
    }
}

/// Adapter: `kithara-beat` NN behind the [`BeatDetector`] seam, built from
/// the embedded small-model bytes.
#[cfg(feature = "beat-nn")]
struct NnDetector {
    inner: BeatThis,
}

#[cfg(feature = "beat-nn")]
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

#[cfg(feature = "beat-nn")]
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
