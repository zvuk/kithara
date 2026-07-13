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
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct BeatAnalysisConfig<B> {
    /// Mono resampler input block size in frames.
    #[builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES)]
    block_frames: usize,
    /// Detector input sample rate in Hz.
    #[builder(default = Consts::DEFAULT_BEAT_TARGET_RATE)]
    target_rate: u32,
    /// Quality used by the configured beat-resampler backend.
    #[builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY)]
    resampler_quality: ResamplerQuality,
    /// Standalone mono resampler backend used before detector windows.
    resampler_backend: B,
    /// Maximum NN detector window length in seconds.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS)]
    detector_window_seconds: u32,
    /// Seconds carried from the end of one detector window into the next.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS)]
    detector_overlap_seconds: u32,
}

impl<B> BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    #[must_use]
    pub fn block_frames(&self) -> usize {
        self.block_frames
    }

    #[must_use]
    pub fn cache_tag(&self) -> Option<String> {
        super::nn::tag(self)
    }

    #[must_use]
    pub fn detector_overlap_seconds(&self) -> u32 {
        self.detector_overlap_seconds
    }

    #[must_use]
    pub fn detector_window_seconds(&self) -> u32 {
        self.detector_window_seconds
    }

    #[must_use]
    pub fn resampler_backend(&self) -> &B {
        &self.resampler_backend
    }

    #[must_use]
    pub fn resampler_quality(&self) -> ResamplerQuality {
        self.resampler_quality
    }

    #[must_use]
    pub fn target_rate(&self) -> u32 {
        self.target_rate
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
        Self {
            block_frames: Consts::DEFAULT_BEAT_BLOCK_FRAMES,
            target_rate: Consts::DEFAULT_BEAT_TARGET_RATE,
            resampler_quality: Consts::DEFAULT_BEAT_RESAMPLER_QUALITY,
            resampler_backend: B::default(),
            detector_window_seconds: Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS,
            detector_overlap_seconds: Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS,
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_resampler::rubato::RubatoBackend;

    use super::BeatAnalysisConfig;

    #[test]
    fn default_beat_config_reports_configured_backend() {
        assert_eq!(
            BeatAnalysisConfig::<RubatoBackend>::default().resampler_backend_name(),
            "rubato"
        );
    }
}
