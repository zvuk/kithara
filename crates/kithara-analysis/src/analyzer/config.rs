use std::fmt;

use bon::Builder;
#[cfg(feature = "beat-nn")]
use kithara_beat::{BeatConfig, BeatConfigPatch};
#[cfg(feature = "beat-dsp")]
use kithara_beat::{Tempo, TempoPatch, TempoPatchError};
use kithara_derive::Patch;
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

/// Beat-analysis tunables used by [`super::AnalyzerBuilder`], beside the
/// resampler backend the caller hands over.
///
/// [`BeatAnalysisConfigPatch`] is what a configuration document may say about
/// it, and [`BeatAnalysisConfigPatchError`] what the merge refuses with. The
/// refusal is declared here rather than read off the fields, so the merge
/// keeps one signature whichever detector the build selects.
#[kithara_config::config(builder = false)]
#[derive(Clone, Builder, fieldwork::Fieldwork, Patch)]
#[builder(state_mod(vis = "pub"))]
#[patch(fallible)]
#[non_exhaustive]
#[fieldwork(get)]
pub struct BeatAnalysisConfig<B> {
    #[config(skip = "injected resampler backend", patch(skip))]
    resampler_backend: B,
    #[config(
        value,
        builder(default = Consts::DEFAULT_BEAT_RESAMPLER_QUALITY),
        field(get(copy))
    )]
    pub resampler_quality: ResamplerQuality,
    #[config(value, builder(default = Consts::DEFAULT_BEAT_DETECTOR_MIN_WINDOW_SECONDS))]
    pub detector_min_window_seconds: u32,
    #[config(value, builder(default = Consts::DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS))]
    pub detector_overlap_seconds: u32,
    #[config(value, builder(default = Consts::DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS))]
    pub detector_window_seconds: u32,
    #[config(value, builder(default = Consts::DEFAULT_BEAT_TARGET_RATE))]
    pub target_rate: u32,
    #[config(value, builder(default = Consts::DEFAULT_BEAT_BLOCK_FRAMES))]
    pub block_frames: usize,
    /// Reaches the detector's peak-picking policy. Nested rather than
    /// flattened so a document can patch `beat:` on its own.
    #[cfg(feature = "beat-nn")]
    #[config(nested, builder(default), field(get(copy)), patch(nested))]
    pub beat: BeatConfig,
    /// The tempo the signal detector searches. A document patches it key by
    /// key under `tempo:`, and [`Tempo`] judges the merged policy as a whole
    /// before it is committed, so a band the comb never scores is refused by
    /// name instead of searched.
    #[cfg(feature = "beat-dsp")]
    #[config(value, builder(default), field(get(copy)), patch(nested, fallible))]
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
        #[cfg(feature = "beat-nn")]
        out.field("beat", &self.beat);
        out
    }

    fn resampler_backend_name(&self) -> &'static str {
        self.resampler_backend.name()
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
    #[cfg(feature = "beat-nn")]
    use kithara_beat::BeatConfig;
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_test_utils::kithara;

    use super::BeatAnalysisConfig;

    #[kithara::test(native, flash(false))]
    fn retained_analysis_values_exclude_the_backend_and_follow_policy() {
        let config = BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .target_rate(48_000)
            .block_frames(2_048)
            .build();
        let values = kithara_config::Config::values(&config);

        assert_eq!(values.target_rate, 48_000);
        assert_eq!(values.block_frames, 2_048);
        #[cfg(feature = "beat-nn")]
        assert_eq!(values.beat.peak_threshold, config.beat.peak_threshold);
    }

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

    #[cfg(feature = "beat-nn")]
    #[kithara::test(native, flash(false))]
    fn a_moved_picking_policy_changes_the_cache_tag() {
        let tag = |beat: BeatConfig| {
            BeatAnalysisConfig::builder()
                .resampler_backend(RubatoBackend::default())
                .beat(beat)
                .build()
                .cache_tag()
                .expect("beat NN has a cache tag")
        };

        assert_ne!(
            tag(BeatConfig::default()),
            tag(BeatConfig::builder().peak_threshold(0.25).build()),
            "a moved peak-picking policy must not share a cached grid"
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    #[cfg(feature = "beat-nn")]
    use kithara_beat::BeatConfig;
    use kithara_resampler::{ResamplerQuality, rubato::RubatoBackend};
    use kithara_test_utils::kithara;

    use super::{BeatAnalysisConfig, BeatAnalysisConfigPatch};
    #[cfg(feature = "beat-dsp")]
    use super::{BeatAnalysisConfigPatchError, Tempo, TempoPatchError};

    fn config() -> BeatAnalysisConfig<RubatoBackend> {
        BeatAnalysisConfig::builder()
            .resampler_backend(RubatoBackend::default())
            .build()
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_fields_it_names() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("target_rate: 48000\n").expect("the document types");
        let mut config = config();
        // Seeded off the crate default (1024) so a whole-struct `apply` that
        // resets every unnamed field cannot pass this assertion by chance.
        config.block_frames = 2048;

        config
            .apply(patch)
            .expect("every key names a value the config accepts");

        assert_eq!(config.target_rate, 48_000);
        assert_eq!(
            config.block_frames, 2048,
            "a silent field must keep the value it already had"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_named_resampler_quality_arrives_as_its_variant() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("resampler_quality: fast\n").expect("the document types");
        let mut config = config();
        config.resampler_quality = ResamplerQuality::Normal;

        config
            .apply(patch)
            .expect("every key names a value the config accepts");

        assert_eq!(
            config.resampler_quality,
            ResamplerQuality::Fast,
            "the document's snake_case name must reach the variant"
        );
    }

    #[cfg(feature = "beat-nn")]
    #[kithara::test(native, flash(false))]
    fn a_nested_beat_patch_reaches_the_inner_field() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("beat:\n  peak_half_width: 5\n").expect("the document types");
        let mut config = config();
        config.beat = BeatConfig::builder().dedup_width(4).build();

        config
            .apply(patch)
            .expect("every key names a value the config accepts");

        assert_eq!(config.beat.peak_half_width, 5);
        assert_eq!(
            config.beat.dedup_width, 4,
            "a silent inner field must keep the value it already had"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<BeatAnalysisConfigPatch>("target_rate_hz: 1\n")
            .expect_err("a typo must not be silently ignored");

        assert!(format!("{error}").contains("target_rate_hz"), "{error}");
    }

    /// `resampler_backend` is the caller's own object, not a value a document
    /// can name (see the patch's doc comment).
    #[kithara::test(native, flash(false))]
    fn the_caller_owned_backend_is_not_a_document_key() {
        let error =
            serde_yaml_ng::from_str::<BeatAnalysisConfigPatch>("resampler_backend: rubato\n")
                .expect_err("a passed object must not be settable from a document");

        assert!(format!("{error}").contains("resampler_backend"), "{error}");
    }

    #[cfg(feature = "beat-dsp")]
    #[kithara::test(native, flash(false))]
    fn a_nested_tempo_patch_reaches_the_searched_band() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("tempo:\n  prior: 100.0\n").expect("the document types");
        let mut config = config();

        config
            .apply(patch)
            .expect("a prior inside the default band");

        assert_eq!(config.tempo().prior(), 100.0);
        assert_eq!(
            config.tempo().band(),
            Tempo::default().band(),
            "a silent inner key must keep the band already in place"
        );
    }

    #[cfg(feature = "beat-dsp")]
    #[kithara::test(native, flash(false))]
    fn a_tempo_the_comb_never_scores_is_refused_under_its_own_key() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("tempo:\n  low: 30.0\n").expect("the document types");
        let mut config = config();

        let error = config
            .apply(patch)
            .expect_err("a band past the scored hypotheses must not merge");

        assert!(
            matches!(
                error,
                BeatAnalysisConfigPatchError::Tempo(TempoPatchError::Invalid(_))
            ),
            "{error}"
        );
        assert!(
            format!("{error}").starts_with("tempo: "),
            "the refusal names the document key that carried it, read as {error}"
        );
    }

    #[cfg(feature = "beat-dsp")]
    #[kithara::test(native, flash(false))]
    fn a_refused_tempo_leaves_every_other_key_of_the_document_uncommitted() {
        let patch: BeatAnalysisConfigPatch =
            serde_yaml_ng::from_str("target_rate: 48000\ntempo:\n  low: 30.0\n")
                .expect("the document types");
        let mut config = config();
        let before = config.target_rate;

        config
            .apply(patch)
            .expect_err("the band is not one the comb scores");

        assert_eq!(
            config.target_rate, before,
            "a refused document commits none of its keys"
        );
    }
}
