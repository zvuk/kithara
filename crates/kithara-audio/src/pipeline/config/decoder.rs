use std::num::NonZeroU32;

use bon::Builder;
use kithara_decode::{DecoderBackend, DecoderResamplerConfig, GaplessMode};
use kithara_derive::Patch;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality};

/// Retained decoder-side resampling choices apart from the injected backend.
#[kithara_config::config(builder = false)]
#[derive(Clone, Debug, Builder, fieldwork::Fieldwork)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(get)]
pub struct DecoderResamplerSettings<B = NoResamplerBackend> {
    /// Backend supplied by the caller for decoder-side rate conversion.
    #[config(skip = "injected resampler backend")]
    pub(crate) backend: B,
    /// Backend-specific conversion options retained for each decoder build.
    #[config(value)]
    #[builder(default)]
    #[field(get(copy))]
    pub(crate) options: ResamplerOptions,
    /// Conversion quality selected for the decoder-side resampler.
    #[config(value)]
    #[builder(default)]
    #[field(get(copy))]
    pub(crate) quality: ResamplerQuality,
}

impl<B> Default for DecoderResamplerSettings<B>
where
    B: Default,
{
    fn default() -> Self {
        Self::builder().backend(B::default()).build()
    }
}

/// Decoder construction settings, including decoder-side resampling.
///
/// [`AudioDecoderConfigPatch`] is what a configuration document may say about
/// it, reached through `audio.decoder`.
#[kithara_config::config(builder = false)]
#[derive(Clone, Debug, Builder, fieldwork::Fieldwork, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
#[fieldwork(opt_in, get)]
#[derive(kithara_derive::BuiltDefault)]
pub struct AudioDecoderConfig<B = NoResamplerBackend> {
    /// Decoder implementation selected for this resource.
    #[config(value, field(get, copy))]
    #[builder(default)]
    pub(crate) backend: DecoderBackend,
    /// Treatment of encoder delay and tail padding.
    #[config(value)]
    #[builder(default)]
    #[field(get, copy)]
    pub(crate) gapless_mode: GaplessMode,
    /// Not a document key: `DecoderResamplerSettings` carries the resampler
    /// backend itself, an object the construction site hands over and no
    /// document can name. `None` means the decoder resamples through
    /// `B::default()` with this crate's own options and quality.
    #[config(value(Option<DecoderResamplerSettingsValues>, self.resampler.as_ref().map(kithara_config::Config::values)))]
    #[patch(skip)]
    pub(crate) resampler: Option<DecoderResamplerSettings<B>>,
}

impl<B> AudioDecoderConfig<B>
where
    B: Default + ResamplerBackend,
{
    pub(crate) fn build_resampler_config(
        &self,
        target_sample_rate: Option<NonZeroU32>,
    ) -> Option<DecoderResamplerConfig<B>> {
        let target_sample_rate = target_sample_rate?;
        let resampler = self.effective_resampler();
        Some(
            DecoderResamplerConfig::builder()
                .target_sample_rate(target_sample_rate)
                .backend(resampler.backend)
                .quality(resampler.quality)
                .options(resampler.options)
                .build(),
        )
    }

    fn effective_resampler(&self) -> DecoderResamplerSettings<B> {
        self.resampler.clone().unwrap_or_default()
    }

    #[must_use]
    pub(crate) fn resampler_backend_name(&self) -> &'static str {
        self.effective_resampler().backend.name()
    }
}

impl<B> AudioDecoderConfig<B> {
    delegate::delegate! {
        to self.resampler {
            /// Return the explicitly configured decoder-side resampler settings.
            #[must_use]
            #[call(as_ref)]
            pub const fn resampler(&self) -> Option<&DecoderResamplerSettings<B>>;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_config::Config as _;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn retained_resampler_recipe_matches_prepared_decoder() {
        let defaults = AudioDecoderConfig::<NoResamplerBackend>::default();
        assert!(defaults.values().resampler.is_none());
        assert!(
            defaults
                .build_resampler_config(NonZeroU32::new(48_000))
                .is_some()
        );

        let options = ResamplerOptions::builder().chunk_size(2_048).build();
        let config = AudioDecoderConfig::builder()
            .gapless_mode(GaplessMode::Disabled)
            .resampler(
                DecoderResamplerSettings::builder()
                    .backend(NoResamplerBackend)
                    .options(options)
                    .quality(ResamplerQuality::Good)
                    .build(),
            )
            .build();

        let values = config.values();
        assert_eq!(values.gapless_mode, GaplessMode::Disabled);
        assert_eq!(values.backend, config.backend());
        let recipe = values.resampler.expect("explicit resampler recipe");
        assert_eq!(recipe.options, options);
        assert_eq!(recipe.quality, ResamplerQuality::Good);

        let prepared = config
            .build_resampler_config(NonZeroU32::new(48_000))
            .expect("resampler target");
        assert_eq!(prepared.options, recipe.options);
        assert_eq!(prepared.quality, recipe.quality);
    }
}
