#[cfg(feature = "analysis-beat")]
use kithara_bufpool::{HasPool, PoolRegion};
#[cfg(feature = "analysis-beat")]
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;

use crate::BeatAnalysisConfig;

#[cfg(feature = "analysis-beat")]
pub(crate) fn detector<S, B>(
    _pools: &PoolRegion<S>,
    _config: &BeatAnalysisConfig<B>,
) -> Option<Arc<dyn crate::beat::BeatDetector>>
where
    S: HasPool<f32> + Send + Sync + 'static,
    B: ResamplerBackend,
{
    None
}

pub(crate) fn tag<B>(_config: &BeatAnalysisConfig<B>) -> Option<String>
where
    B: ResamplerBackend,
{
    None
}
