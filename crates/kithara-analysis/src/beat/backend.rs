use kithara_beat::{BEAT_MODEL_BYTES, BeatConfig, BeatThis, MEL_MODEL_BYTES};
use kithara_bufpool::{HasPool, PoolRegion};

use super::{BeatDetectError, BeatDetector, BeatMark, RawBeats};

#[derive(Debug, Clone, Copy, derive_more::Display, PartialEq, Eq)]
#[display("{self:?}")]
pub(crate) enum BeatDetectorKind {
    NnBeatThis,
}

impl BeatDetectorKind {
    pub(crate) const ALL: &'static [Self] = &[Self::NnBeatThis];

    pub(crate) fn first() -> Self {
        Self::ALL[0]
    }
}

impl Default for BeatDetectorKind {
    fn default() -> Self {
        Self::first()
    }
}

pub(crate) fn build_detector<S>(
    kind: BeatDetectorKind,
    pools: &PoolRegion<S>,
    config: BeatConfig,
) -> Result<Box<dyn BeatDetector>, BeatDetectError>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    match kind {
        BeatDetectorKind::NnBeatThis => Ok(Box::new(NnDetector::new(pools, config)?)),
    }
}

struct NnDetector<S>
where
    S: HasPool<f32>,
{
    inner: BeatThis<S>,
}

impl<S> NnDetector<S>
where
    S: HasPool<f32>,
{
    fn new(pools: &PoolRegion<S>, config: BeatConfig) -> Result<Self, BeatDetectError> {
        let inner = BeatThis::builder()
            .mel_model(MEL_MODEL_BYTES)
            .beat_model(BEAT_MODEL_BYTES)
            .pools(pools.clone())
            .config(config)
            .build()
            .map_err(|e| BeatDetectError::Init {
                reason: e.to_string(),
            })?;
        Ok(Self { inner })
    }
}

impl<S> BeatDetector for NnDetector<S>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    fn detect(&self, mono_window: &[f32]) -> Result<RawBeats, BeatDetectError> {
        let raw = self
            .inner
            .analyze(mono_window)
            .map_err(|e| BeatDetectError::Detect {
                reason: e.to_string(),
            })?;
        Ok(RawBeats {
            beats: raw.beats.into_iter().map(mark).collect(),
            downbeats: raw.downbeats.into_iter().map(mark).collect(),
        })
    }
}

fn mark(mark: kithara_beat::BeatMark) -> BeatMark {
    BeatMark {
        at: mark.at,
        confidence: mark.confidence,
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;
    use num_traits::cast::AsPrimitive;

    use super::{BeatConfig, BeatDetectorKind, build_detector};
    use crate::test_pools::pools;

    struct Consts;

    impl Consts {
        const SAMPLE_RATE: usize = 22_050;
        const SECONDS: usize = 2;
    }

    fn tone() -> Vec<f32> {
        let rate: f32 = Consts::SAMPLE_RATE.as_();
        let step = std::f32::consts::TAU * 220.0 / rate;
        (0..Consts::SECONDS * Consts::SAMPLE_RATE)
            .map(|n| {
                let t: f32 = n.as_();
                0.5 * (step * t).sin()
            })
            .collect()
    }

    #[kithara::test(native, flash(false))]
    fn a_non_default_beat_config_reaches_the_picker() {
        let pcm = tone();

        let suppressed = BeatConfig::builder().peak_threshold(f32::MAX).build();
        let detector = build_detector(BeatDetectorKind::default(), &pools(), suppressed)
            .unwrap_or_else(|e| panic!("suppressed detector init failed: {e}"));
        let raw = detector
            .detect(&pcm)
            .unwrap_or_else(|e| panic!("suppressed detect failed: {e}"));
        assert!(
            raw.beats.is_empty() && raw.downbeats.is_empty(),
            "a threshold above every possible logit must admit no peaks"
        );

        let admit_all = BeatConfig::builder()
            .peak_threshold(f32::MIN)
            .peak_half_width(0)
            .dedup_width(0)
            .build();
        let detector = build_detector(BeatDetectorKind::default(), &pools(), admit_all)
            .unwrap_or_else(|e| panic!("admit-all detector init failed: {e}"));
        let raw = detector
            .detect(&pcm)
            .unwrap_or_else(|e| panic!("admit-all detect failed: {e}"));
        assert!(
            !raw.beats.is_empty() && !raw.downbeats.is_empty(),
            "a threshold below every possible logit, with no suppression window, must admit a peak at every frame"
        );
    }
}
