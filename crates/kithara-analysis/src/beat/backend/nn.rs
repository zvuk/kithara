use kithara_beat::{BEAT_MODEL_BYTES, BeatDetectError, BeatThis, MEL_MODEL_BYTES};
use kithara_bufpool::{HasPool, PoolRegion};

use crate::BeatAnalysisConfig;

#[cfg(not(feature = "beat-cuda"))]
pub(super) fn detector<B, S>(
    config: &BeatAnalysisConfig<B>,
    pools: &PoolRegion<S>,
) -> Result<BeatThis<S>, BeatDetectError>
where
    S: HasPool<f32>,
{
    BeatThis::builder()
        .mel_model(MEL_MODEL_BYTES)
        .beat_model(BEAT_MODEL_BYTES)
        .pools(pools.clone())
        .config(config.beat().clone())
        .build()
        .map_err(|e| BeatDetectError::Init {
            reason: e.to_string(),
        })
}

#[cfg(feature = "beat-cuda")]
pub(super) fn detector<B, S>(
    config: &BeatAnalysisConfig<B>,
    pools: &PoolRegion<S>,
) -> Result<BeatThis<S>, BeatDetectError>
where
    S: HasPool<f32>,
{
    BeatThis::builder()
        .mel_model(MEL_MODEL_BYTES)
        .beat_model(BEAT_MODEL_BYTES)
        .pools(pools.clone())
        .config(config.beat().clone())
        .maybe_environment(config.cuda_environment().as_ref())
        .build()
        .map_err(|e| BeatDetectError::Init {
            reason: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use kithara_beat::BeatConfig;
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_fixtures::analysis_beat_fixtures::nn_tone;
    use kithara_test_utils::{bufpool::pools, kithara};

    use super::super::{BeatDetectorKind, build_detector};
    use crate::BeatAnalysisConfig;

    fn config(beat: BeatConfig) -> BeatAnalysisConfig<RubatoBackend> {
        BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .beat(beat)
            .build()
    }

    #[kithara::test(native, flash(false))]
    fn a_non_default_beat_config_reaches_the_picker(nn_tone: Vec<f32>) {
        let pcm = nn_tone;

        let suppressed = config(BeatConfig::builder().peak_threshold(f32::MAX).build());
        let detector = build_detector(BeatDetectorKind::NnBeatThis, &suppressed, &pools())
            .unwrap_or_else(|e| panic!("suppressed detector init failed: {e}"));
        let raw = detector
            .detect(&pcm)
            .unwrap_or_else(|e| panic!("suppressed detect failed: {e}"));
        assert!(
            raw.beats.is_empty() && raw.downbeats.is_empty(),
            "a threshold above every possible logit must admit no peaks"
        );

        let admit_all = config(
            BeatConfig::builder()
                .peak_threshold(f32::MIN)
                .peak_half_width(0)
                .dedup_width(0)
                .build(),
        );
        let detector = build_detector(BeatDetectorKind::NnBeatThis, &admit_all, &pools())
            .unwrap_or_else(|e| panic!("admit-all detector init failed: {e}"));
        let raw = detector
            .detect(&pcm)
            .unwrap_or_else(|e| panic!("admit-all detect failed: {e}"));
        assert!(
            !raw.beats.is_empty() && !raw.downbeats.is_empty(),
            "a threshold below every possible logit, with no suppression window, must admit a peak at every frame"
        );
    }

    #[cfg(feature = "beat-cuda")]
    #[kithara::test(native, flash(false))]
    #[ignore = "requires native GPU ONNX Runtime and NVIDIA CUDA/cuDNN"]
    fn the_caller_environment_reaches_cuda_detector_construction(nn_tone: Vec<f32>) {
        let beat = BeatConfig::builder()
            .backend(kithara_beat::BeatBackend::Cuda { device_id: 0 })
            .build();
        let absent = build_detector(
            BeatDetectorKind::NnBeatThis,
            &config(beat.clone()),
            &pools(),
        );
        assert!(
            matches!(absent, Err(kithara_beat::BeatDetectError::Init { reason })
                if reason == kithara_beat::BeatError::CudaEnvironmentMissing.to_string()),
            "CUDA selection cannot silently initialize an environment or select CPU"
        );
        let environment = kithara_beat::ort::init_from(beat.ort_library_path())
            .expect("configured CUDA runtime library")
            .build()
            .expect("caller-owned native environment");
        let configured = |beat| {
            BeatAnalysisConfig::builder()
                .resampler_backend(RubatoBackend::default())
                .beat(beat)
                .cuda_environment(environment.clone())
                .build()
        };
        let suppressed = configured(
            BeatConfig::builder()
                .backend(kithara_beat::BeatBackend::Cuda { device_id: 0 })
                .peak_threshold(f32::MAX)
                .build(),
        );
        let detector = build_detector(BeatDetectorKind::NnBeatThis, &suppressed, &pools())
            .expect("CUDA detector with caller environment");
        let raw = detector.detect(&nn_tone).expect("CUDA suppressed analysis");
        assert!(
            raw.beats.is_empty() && raw.downbeats.is_empty(),
            "a threshold above every possible logit must admit no peaks"
        );
        let admit_all = configured(
            BeatConfig::builder()
                .backend(kithara_beat::BeatBackend::Cuda { device_id: 0 })
                .peak_threshold(f32::MIN)
                .peak_half_width(0)
                .dedup_width(0)
                .build(),
        );
        let detector = build_detector(BeatDetectorKind::NnBeatThis, &admit_all, &pools())
            .expect("CUDA admit-all detector with caller environment");
        let raw = detector.detect(&nn_tone).expect("CUDA admit-all analysis");
        assert!(
            !raw.beats.is_empty() && !raw.downbeats.is_empty(),
            "a threshold below every possible logit, with no suppression window, must admit a peak at every frame"
        );
    }
}
