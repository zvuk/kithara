use kithara_beat::SpectralBeats;
use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::detector::{BeatDetectError, BeatDetector, RawBeats},
    build::marks,
};
use crate::BeatAnalysisConfig;

pub(super) fn detector<B, S>(
    config: &BeatAnalysisConfig<B>,
    pools: &PoolRegion<S>,
) -> Result<SpectralBeats<S>, BeatDetectError>
where
    S: HasPool<f32>,
{
    Ok(SpectralBeats::new(pools.clone(), config.tempo())?)
}

impl<S> BeatDetector for SpectralBeats<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        Ok(marks(self.analyze(mono_window)?))
    }
}
