use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};
use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::{BeatDetectError, BeatDetector, RawBeats},
    build::marks,
};

pub(super) struct Detector<S>
where
    S: HasPool<f32>,
{
    inner: BeatThis<S>,
}

impl<S> Detector<S>
where
    S: HasPool<f32>,
{
    pub(super) fn new(pools: &PoolRegion<S>) -> Result<Self, BeatDetectError> {
        let inner = BeatThis::builder()
            .mel_model(MEL_MODEL_BYTES)
            .beat_model(BEAT_MODEL_BYTES)
            .pools(pools.clone())
            .build()
            .map_err(|e| BeatDetectError::Init {
                reason: e.to_string(),
            })?;
        Ok(Self { inner })
    }
}

impl<S> BeatDetector for Detector<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        self.inner
            .analyze(mono_window)
            .map(marks)
            .map_err(|e| BeatDetectError::Detect {
                reason: e.to_string(),
            })
    }
}
