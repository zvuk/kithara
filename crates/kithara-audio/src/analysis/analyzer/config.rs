use std::fmt;

use bon::Builder;
use kithara_resampler::{ResamplerBackend, ResamplerQuality};

struct Consts;

impl Consts {
    const DEFAULT_BEAT_BLOCK_FRAMES: usize = 1024;
    const DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS: u32 = 2;
    const DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS: u32 = 30;
    const DEFAULT_BEAT_RESAMPLER_QUALITY: ResamplerQuality = ResamplerQuality::High;
    const DEFAULT_BEAT_TARGET_RATE: u32 = 22_050;
}

/// Beat-analysis tunables used by [`super::AnalyzerBuilder`].
#[derive(Clone, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(get)]
pub struct BeatAnalysisConfig<B> {
    /// Standalone mono resampler backend used before detector windows.
    resampler_backend: B,
    /// Quality used by the configured beat-resampler backend.
    #[builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY)]
    #[field(get(copy))]
    resampler_quality: ResamplerQuality,
    /// Seconds carried from the end of one detector window into the next.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS)]
    detector_overlap_seconds: u32,
    /// Maximum NN detector window length in seconds.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS)]
    detector_window_seconds: u32,
    /// Detector input sample rate in Hz.
    #[builder(default = Consts::DEFAULT_BEAT_TARGET_RATE)]
    target_rate: u32,
    /// Mono resampler input block size in frames.
    #[builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES)]
    block_frames: usize,
}

impl<B> BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    #[must_use]
    pub fn cache_tag(&self) -> Option<String> {
        super::nn::tag(self)
    }

    fn resampler_backend_name(&self) -> &'static str {
        self.resampler_backend.name()
    }
}

impl<B> fmt::Debug for BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeatAnalysisConfig")
            .field("block_frames", &self.block_frames)
            .field("target_rate", &self.target_rate)
            .field("resampler_quality", &self.resampler_quality)
            .field("resampler_backend", &self.resampler_backend_name())
            .field("detector_window_seconds", &self.detector_window_seconds)
            .field("detector_overlap_seconds", &self.detector_overlap_seconds)
            .finish()
    }
}

impl<B> Default for BeatAnalysisConfig<B>
where
    B: ResamplerBackend + Default,
{
    fn default() -> Self {
        Self::builder().resampler_backend(B::default()).build()
    }
}

#[cfg(test)]
mod tests {
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;

    use super::BeatAnalysisConfig;

    #[kithara::test(native, flash(false))]
    fn default_beat_config_reports_configured_backend() {
        assert_eq!(
            BeatAnalysisConfig::<RubatoBackend>::default().resampler_backend_name(),
            "rubato"
        );
    }

    #[cfg(feature = "beat-nn")]
    #[kithara::test(native, flash(false))]
    fn cache_tag_invalidates_pre_bpm_from_beats_results() {
        let tag = BeatAnalysisConfig::<RubatoBackend>::default()
            .cache_tag()
            .expect("beat NN has a cache tag");

        assert!(
            tag.contains(":grid_bpm_from_beats_v1:"),
            "grid semantics must participate in durable-cache identity"
        );
    }
}
