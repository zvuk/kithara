use std::fmt;

use bon::Builder;
use kithara_resampler::{ResamplerBackendConfig, ResamplerQuality};

struct Consts;

impl Consts {
    const DEFAULT_BEAT_BLOCK_FRAMES: usize = 1024;
    const DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS: u32 = 2;
    const DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS: u32 = 30;
    const DEFAULT_BEAT_RESAMPLER_QUALITY: ResamplerQuality = ResamplerQuality::High;
    const DEFAULT_BEAT_TARGET_RATE: u32 = 22_050;
}

pub type BeatResamplerBackend = ResamplerBackendConfig;

/// Beat-analysis tunables used by [`super::AnalyzerBuilder`].
#[derive(Clone, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct BeatAnalysisConfig {
    /// Mono resampler input block size in frames.
    #[builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES)]
    pub block_frames: usize,
    /// Detector input sample rate in Hz.
    #[builder(default = Consts::DEFAULT_BEAT_TARGET_RATE)]
    pub target_rate: u32,
    /// Quality used by the configured beat-resampler backend.
    #[builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY)]
    pub resampler_quality: ResamplerQuality,
    /// Standalone mono resampler backend used before detector windows.
    #[builder(default = default_beat_resampler_backend())]
    pub resampler_backend: BeatResamplerBackend,
    /// Maximum NN detector window length in seconds.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS)]
    pub detector_window_seconds: u32,
    /// Seconds carried from the end of one detector window into the next.
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS)]
    pub detector_overlap_seconds: u32,
}

impl BeatAnalysisConfig {
    #[must_use]
    pub fn cache_tag(&self) -> Option<String> {
        super::nn::tag(self)
    }

    fn resampler_backend_name(&self) -> Option<&'static str> {
        self.resampler_backend.name()
    }
}

impl fmt::Debug for BeatAnalysisConfig {
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

impl Default for BeatAnalysisConfig {
    fn default() -> Self {
        Self {
            block_frames: Consts::DEFAULT_BEAT_BLOCK_FRAMES,
            target_rate: Consts::DEFAULT_BEAT_TARGET_RATE,
            resampler_quality: Consts::DEFAULT_BEAT_RESAMPLER_QUALITY,
            resampler_backend: default_beat_resampler_backend(),
            detector_window_seconds: Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS,
            detector_overlap_seconds: Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS,
        }
    }
}

#[cfg(feature = "analysis-beat")]
fn default_beat_resampler_backend() -> BeatResamplerBackend {
    BeatResamplerBackend::default()
}

#[cfg(not(feature = "analysis-beat"))]
fn default_beat_resampler_backend() -> BeatResamplerBackend {
    BeatResamplerBackend::none()
}

#[cfg(test)]
mod tests {
    use super::BeatAnalysisConfig;

    #[test]
    fn default_beat_config_uses_compiled_backend_order() {
        let expected = if cfg!(all(feature = "analysis-beat", feature = "resample-rubato")) {
            Some("rubato")
        } else if cfg!(all(
            feature = "analysis-beat",
            feature = "resample-readhead"
        )) {
            Some("read-head")
        } else {
            None
        };

        assert_eq!(
            BeatAnalysisConfig::default().resampler_backend_name(),
            expected
        );
    }
}
