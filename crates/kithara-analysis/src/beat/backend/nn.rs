use kithara_beat::{BEAT_MODEL_BYTES, BeatThis, MEL_MODEL_BYTES};
use kithara_bufpool::{HasPool, PoolRegion};

use super::{
    super::detector::{BeatDetectError, BeatDetector, RawBeats},
    build::marks,
};
use crate::BeatAnalysisConfig;

pub(super) fn detector<B, S>(
    _config: &BeatAnalysisConfig<B>,
    pools: &PoolRegion<S>,
) -> Result<BeatThis<S>, BeatDetectError>
where
    S: HasPool<f32>,
{
    BeatThis::builder()
        .mel_model(MEL_MODEL_BYTES)
        .beat_model(BEAT_MODEL_BYTES)
        .pools(pools.clone())
        .build()
        .map_err(|e| BeatDetectError::Init {
            reason: e.to_string(),
        })
}

impl<S> BeatDetector for BeatThis<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        self.analyze(mono_window)
            .map(marks)
            .map_err(|e| BeatDetectError::Detect {
                reason: e.to_string(),
            })
    }
}
