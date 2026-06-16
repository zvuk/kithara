//! `beat-nn` cfg seam: the only site that knows whether an NN detector is
//! compiled in. [`super::set`] stays cfg-free above it.

#[cfg(feature = "beat-nn")]
mod model {
    /// Embedded NN model identity for the cache tag.
    pub(super) const NN_MODEL_TAG: &str = "beat_this_small_v1";
}

/// Build the shared NN detector once. `None` when its model fails to load.
#[cfg(feature = "beat-nn")]
pub(crate) fn detector() -> Option<crate::analysis::beat::SharedBeatDetector> {
    use std::sync::Arc;

    use kithara_platform::sync::Mutex;
    use tracing::warn;

    use crate::analysis::beat::{BeatDetectorKind, build_detector};

    match build_detector(BeatDetectorKind::default()) {
        Ok(detector) => Some(Arc::new(Mutex::new(detector))),
        Err(e) => {
            warn!(?e, "beat detector init failed; beat analysis disabled");
            None
        }
    }
}

/// Cache fingerprint of the compiled-in detector + grid tuning. `None` when no
/// detector is compiled in.
pub(crate) fn tag() -> Option<String> {
    #[cfg(feature = "beat-nn")]
    {
        use crate::analysis::beat::{BeatDetectorKind, GridParams};

        // The first compiled-in detector is the `Default` the builder uses.
        BeatDetectorKind::ALL
            .first()
            .map(|kind| format!("{kind}:{}:{:?}", model::NN_MODEL_TAG, GridParams::default()))
    }
    #[cfg(not(feature = "beat-nn"))]
    {
        None
    }
}
