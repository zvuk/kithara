use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use crate::{
    BeatAnalysisConfig,
    beat::{BeatDetectorKind, GRID_SEMANTICS_TAG, GridParams, build_detector},
};

const NN_MODEL_TAG: &str = "beat_this_small_v1";

pub(crate) fn detector<S, B>(
    pools: &PoolRegion<S>,
    config: &BeatAnalysisConfig<B>,
) -> Option<Arc<dyn crate::beat::BeatDetector>>
where
    S: HasPool<f32> + Send + Sync + 'static,
    B: ResamplerBackend,
{
    match build_detector(BeatDetectorKind::default(), pools, config.beat()) {
        Ok(detector) => Some(Arc::from(detector)),
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
            "{kind}:{NN_MODEL_TAG}:{}:{:?}:{:?}",
            GRID_SEMANTICS_TAG,
            GridParams::default(),
            config
        )
    })
}
