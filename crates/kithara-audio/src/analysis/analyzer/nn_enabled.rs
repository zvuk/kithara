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
    /// Embedded NN model identity for the cache tag.
    pub(super) const NN_MODEL_TAG: &str = "beat_this_small_v1";
}

/// Build the shared NN detector once. `None` when its model fails to load.
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

#[cfg(not(feature = "beat-nn"))]
pub(crate) fn detector() -> Option<crate::analysis::beat::SharedBeatDetector> {
    None
}

/// Cache fingerprint of the compiled-in detector + grid tuning. `None` when no
/// detector is compiled in.
#[cfg(feature = "beat-nn")]
pub(crate) fn tag() -> Option<String> {
    // The first compiled-in detector is the `Default` the builder uses.
    BeatDetectorKind::ALL
        .first()
        .map(|kind| format!("{kind}:{}:{:?}", model::NN_MODEL_TAG, GridParams::default()))
}

#[cfg(not(feature = "beat-nn"))]
pub(crate) fn tag() -> Option<String> {
    None
}
