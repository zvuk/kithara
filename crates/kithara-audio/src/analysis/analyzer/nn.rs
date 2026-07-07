#[cfg(feature = "beat-nn")]
use std::sync::Arc;

#[cfg(feature = "beat-nn")]
use kithara_platform::sync::Mutex;
#[cfg(feature = "beat-nn")]
use tracing::warn;

#[cfg(feature = "beat-nn")]
use crate::analysis::beat::{BeatDetectorKind, GridParams, build_detector};

#[cfg(feature = "beat-nn")]
mod model {
    pub(super) const NN_MODEL_TAG: &str = "beat_this_small_v1";
}

#[cfg(feature = "beat-nn")]
pub(crate) fn detector() -> Option<crate::analysis::beat::SharedBeatDetector> {
    match build_detector(BeatDetectorKind::default()) {
        Ok(detector) => Some(Arc::new(Mutex::new(detector))),
        Err(e) => {
            warn!(?e, "beat detector init failed; beat analysis disabled");
            None
        }
    }
}

#[cfg(all(feature = "analysis-beat", not(feature = "beat-nn")))]
pub(crate) fn detector() -> Option<crate::analysis::beat::SharedBeatDetector> {
    None
}

#[cfg(feature = "beat-nn")]
pub(crate) fn tag() -> Option<String> {
    BeatDetectorKind::ALL
        .first()
        .map(|kind| format!("{kind}:{}:{:?}", model::NN_MODEL_TAG, GridParams::default()))
}

#[cfg(not(feature = "beat-nn"))]
pub(crate) fn tag() -> Option<String> {
    None
}
