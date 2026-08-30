use kithara_beat::SpectralBeats;
use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::{BeatDetectError, BeatDetector, RawBeats},
    build::marks,
};

pub(super) struct Detector<S>
where
    S: HasPool<f32>,
{
    inner: SpectralBeats<S>,
}

impl<S> Detector<S>
where
    S: HasPool<f32>,
{
    pub(super) fn new(pools: &PoolRegion<S>) -> Result<Self, BeatDetectError> {
        Ok(Self {
            inner: SpectralBeats::new(pools.clone())?,
        })
    }
}

impl<S> BeatDetector for Detector<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        Ok(marks(self.inner.analyze(mono_window)?))
    }
}
