use kithara_beat::SpectralBeats;
use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::detector::{BeatDetectError, BeatDetector, RawBeats},
    build::marks,
};

pub(super) fn detector<S>(pools: &PoolRegion<S>) -> Result<SpectralBeats<S>, BeatDetectError>
where
    S: HasPool<f32>,
{
    Ok(SpectralBeats::new(pools.clone())?)
}

impl<S> BeatDetector for SpectralBeats<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        Ok(marks(self.analyze(mono_window)?))
    }
}
