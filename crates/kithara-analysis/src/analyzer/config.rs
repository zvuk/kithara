use std::fmt;

use bon::Builder;
#[cfg(feature = "beat-dsp")]
use kithara_beat::Tempo;
use kithara_resampler::{ResamplerBackend, ResamplerQuality};

struct Consts;

impl Consts {
    const DEFAULT_BEAT_BLOCK_FRAMES: usize = 1024;
    const DEFAULT_BEAT_DETECTOR_MIN_WINDOW_SECONDS: u32 = 10;
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
    resampler_backend: B,
    #[builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY)]
    #[field(get(copy))]
    resampler_quality: ResamplerQuality,
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_MIN_WINDOW_SECONDS)]
    detector_min_window_seconds: u32,
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS)]
    detector_overlap_seconds: u32,
    #[builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS)]
    detector_window_seconds: u32,
    #[builder(default = Consts::DEFAULT_BEAT_TARGET_RATE)]
    target_rate: u32,
    #[builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES)]
    block_frames: usize,
    #[cfg(feature = "beat-dsp")]
    #[builder(default)]
    #[field(get(copy))]
    tempo: Tempo,
}

#[cfg(feature = "beat-backend")]
impl<B> BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    /// What a cached analysis must have been produced under to be served back.
    #[must_use]
    pub fn cache_tag(&self) -> Option<String> {
        Some(crate::model::tag(self))
    }
}

#[cfg(not(feature = "beat-backend"))]
impl<B> BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    /// What a cached analysis must have been produced under to be served back.
    #[must_use]
    pub const fn cache_tag(&self) -> Option<String> {
        None
    }
}

impl<B> BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    fn resampler_backend_name(&self) -> &'static str {
        self.resampler_backend.name()
    }

    fn debug_fields<'f, 'a>(&self, f: &'f mut fmt::Formatter<'a>) -> fmt::DebugStruct<'f, 'a> {
        let mut out = f.debug_struct("BeatAnalysisConfig");
        out.field("block_frames", &self.block_frames)
            .field("target_rate", &self.target_rate)
            .field("resampler_quality", &self.resampler_quality)
            .field("resampler_backend", &self.resampler_backend_name())
            .field(
                "detector_min_window_seconds",
                &self.detector_min_window_seconds,
            )
            .field("detector_window_seconds", &self.detector_window_seconds)
            .field("detector_overlap_seconds", &self.detector_overlap_seconds);
        out
    }
}

#[cfg(feature = "beat-dsp")]
impl<B> fmt::Debug for BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.debug_fields(f).field("tempo", &self.tempo).finish()
    }
}

#[cfg(not(feature = "beat-dsp"))]
impl<B> fmt::Debug for BeatAnalysisConfig<B>
where
    B: ResamplerBackend,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.debug_fields(f).finish()
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

    #[cfg(feature = "beat-dsp")]
    #[kithara::test(native, flash(false))]
    fn the_cache_tag_carries_the_tempo_the_detector_searches() {
        let tag = |tempo| {
            BeatAnalysisConfig::builder()
                .resampler_backend(RubatoBackend::default())
                .tempo(tempo)
                .build()
                .cache_tag()
                .expect("a build with a detector has a cache tag")
        };
        let searched = kithara_beat::Tempo::default();
        let narrowed = kithara_beat::Tempo::builder()
            .band(90.0..=180.0)
            .prior(120.0)
            .build()
            .expect("a searchable band");
        let tolerant = kithara_beat::Tempo::builder()
            .tolerance(searched.tolerance() * 2.0)
            .build()
            .expect("a finite positive duration");
        let drifting = kithara_beat::Tempo::builder()
            .drift(30.0)
            .build()
            .expect("a finite positive rate");

        assert_ne!(
            tag(searched),
            tag(narrowed),
            "two search bands are two grids, and one cache tag must not serve both"
        );
        assert_ne!(
            tag(searched),
            tag(tolerant),
            "two beat tolerances are two grids, and one cache tag must not serve both"
        );
        assert_ne!(
            tag(searched),
            tag(drifting),
            "two tempo drifts are two grids, and one cache tag must not serve both"
        );
    }

    #[cfg(feature = "beat-nn")]
    #[kithara::test(native, flash(false))]
    fn the_cache_tag_carries_what_decides_the_grid() {
        let tag = BeatAnalysisConfig::<RubatoBackend>::default()
            .cache_tag()
            .expect("beat NN has a cache tag");

        assert!(
            tag.contains(":grid_bpm_from_beats_v4:"),
            "grid semantics must participate in durable-cache identity"
        );
        assert!(
            !tag.contains(":grid_bpm_from_beats_v3:"),
            "a grid at the level the detector reports is not the grid v3 cached"
        );
        assert!(
            tag.contains(":detector_audio_seamless_v2:"),
            "how the detector was fed decides the grid, so it must decide the tag"
        );
        assert!(
            !tag.contains(":detector_audio_seamless_v1:"),
            "a grid built from a track read whole is not the grid v1 cached"
        );
    }
}
