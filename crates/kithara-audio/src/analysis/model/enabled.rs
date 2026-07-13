use kithara_platform::sync::{Arc, Mutex};
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use crate::analysis::{
    BeatAnalysisConfig,
    beat::{BeatDetectorKind, GridParams, SharedBeatDetector, build_detector},
};

const NN_MODEL_TAG: &str = "beat_this_small_v1";

pub(crate) fn detector() -> Option<SharedBeatDetector> {
    match build_detector(BeatDetectorKind::default()) {
        Ok(detector) => Some(Arc::new(Mutex::new(detector))),
        Err(e) => {
            warn!(?e, "beat detector init failed; beat analysis disabled");
            None
        }
    }
}

pub(crate) fn tag<B>(config: &BeatAnalysisConfig<B>) -> Option<String>
where
    B: ResamplerBackend,
{
    BeatDetectorKind::ALL.first().map(|kind| {
        format!(
            "{kind}:{NN_MODEL_TAG}:{:?}:{:?}",
            GridParams::default(),
            config
        )
    })
}
