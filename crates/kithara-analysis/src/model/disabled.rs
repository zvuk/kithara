#[cfg(feature = "analysis-beat")]
use kithara_bufpool::{HasPool, PoolRegion};
#[cfg(feature = "analysis-beat")]
use kithara_platform::sync::Arc;

#[cfg(feature = "analysis-beat")]
use crate::BeatAnalysisConfig;

#[cfg(feature = "analysis-beat")]
pub(crate) fn detector<B, S>(
    _config: &BeatAnalysisConfig<B>,
    _pools: &PoolRegion<S>,
) -> Option<Arc<dyn crate::beat::BeatDetector>>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    None
}
