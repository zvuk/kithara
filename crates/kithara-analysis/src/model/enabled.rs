use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::sync::Arc;
use kithara_resampler::ResamplerBackend;
use tracing::warn;

use crate::{
    BeatAnalysisConfig,
    beat::{BeatDetectorKind, GRID_SEMANTICS_TAG, GridParams, SELECTED_DETECTOR, build_detector},
};

pub(crate) fn detector<S>(pools: &PoolRegion<S>) -> Option<Arc<dyn crate::beat::BeatDetector>>
where
    S: HasPool<f32> + Send + Sync + 'static,
{
    match build_detector(SELECTED_DETECTOR, pools) {
        Ok(detector) => Some(Arc::from(detector)),
        Err(e) => {
            warn!(?e, "beat detector init failed; beat analysis disabled");
            None
        }
    }
}

/// Names the detector this build selected, so a cached grid is only served
/// back to a build that would have produced it.
pub(crate) fn tag<B>(config: &BeatAnalysisConfig<B>) -> String
where
    B: ResamplerBackend,
{
    tag_for(SELECTED_DETECTOR, config)
}

fn tag_for<B>(kind: BeatDetectorKind, config: &BeatAnalysisConfig<B>) -> String
where
    B: ResamplerBackend,
{
    format!(
        "{kind}:{}:{}:{:?}:{:?}",
        kind.model_tag(),
        GRID_SEMANTICS_TAG,
        GridParams::default(),
        config
    )
}

#[cfg(test)]
mod tests {
    use kithara_resampler::NoResamplerBackend;
    use kithara_test_utils::kithara;

    use super::*;

    fn config() -> BeatAnalysisConfig<NoResamplerBackend> {
        BeatAnalysisConfig::builder()
            .resampler_backend(NoResamplerBackend)
            .build()
    }

    #[kithara::test(native, flash(false))]
    fn the_tag_names_the_detector_that_was_built() {
        let pool = SamplePool::default();
        assert!(
            build_detector(SELECTED_DETECTOR, &pool).is_ok(),
            "the selected detector is the one a build can construct"
        );
        let tag = tag(&config());
        assert!(
            tag.starts_with(&format!("{SELECTED_DETECTOR}:")),
            "the fingerprint names the detector that ran, not another: {tag}"
        );
    }

    #[cfg(all(feature = "beat-nn", feature = "beat-dsp"))]
    #[kithara::test(native, flash(false))]
    fn each_detector_fingerprints_differently() {
        let config = config();
        assert_ne!(
            tag_for(BeatDetectorKind::NnBeatThis, &config),
            tag_for(BeatDetectorKind::DspSpectral, &config),
            "two backends must not share a fingerprint, or a grid from one is served to the other"
        );
    }

    #[cfg(all(feature = "beat-nn", feature = "beat-dsp"))]
    #[kithara::test(native, flash(false))]
    fn a_build_carrying_both_uses_the_network() {
        assert_eq!(SELECTED_DETECTOR, BeatDetectorKind::NnBeatThis);
    }
}
