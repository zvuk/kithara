use std::num::NonZeroU32;

use bon::Builder;
use kithara_decode::{DecoderBackend, DecoderResamplerConfig, GaplessMode};
use kithara_platform::time::Duration;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality};

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct DecoderResamplerSettings<B = NoResamplerBackend> {
    pub(crate) backend: B,
    #[builder(default)]
    pub(crate) options: ResamplerOptions,
    #[builder(default)]
    pub(crate) quality: ResamplerQuality,
}

impl<B> DecoderResamplerSettings<B> {
    /// Return the selected resampler backend.
    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Return the resampler options.
    #[must_use]
    pub fn options(&self) -> ResamplerOptions {
        self.options
    }

    /// Return the resampler quality.
    #[must_use]
    pub fn quality(&self) -> ResamplerQuality {
        self.quality
    }
}

impl<B> Default for DecoderResamplerSettings<B>
where
    B: Default,
{
    fn default() -> Self {
        Self {
            backend: B::default(),
            options: ResamplerOptions::default(),
            quality: ResamplerQuality::default(),
        }
    }
}

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AudioDecoderConfig<B = NoResamplerBackend> {
    #[builder(default)]
    pub(crate) backend: DecoderBackend,
    #[builder(default)]
    pub(crate) gapless_mode: GaplessMode,
    #[builder(default = Duration::from_millis(10))]
    pub(crate) blend_duration: Duration,
    pub(crate) resampler: Option<DecoderResamplerSettings<B>>,
}

impl<B> AudioDecoderConfig<B>
where
    B: ResamplerBackend,
{
    /// Return the decoder backend.
    #[must_use]
    pub fn backend(&self) -> DecoderBackend {
        self.backend
    }

    /// Return the gapless playback mode.
    #[must_use]
    pub fn gapless_mode(&self) -> GaplessMode {
        self.gapless_mode
    }

    /// Return the configured decoder-transition overlap duration.
    #[must_use]
    pub fn blend_duration(&self) -> Duration {
        self.blend_duration
    }

    /// Return the decoder-side resampler settings.
    #[must_use]
    pub fn resampler(&self) -> Option<&DecoderResamplerSettings<B>> {
        self.resampler.as_ref()
    }

    pub(crate) fn build_resampler_config(
        &self,
        target_sample_rate: Option<NonZeroU32>,
    ) -> Result<Option<DecoderResamplerConfig<B>>, kithara_decode::DecodeError> {
        let Some(target_sample_rate) = target_sample_rate else {
            return Ok(None);
        };
        let Some(resampler) = self.resampler.as_ref() else {
            return Ok(None);
        };
        DecoderResamplerConfig::for_decoder_backend(
            self.backend,
            target_sample_rate,
            resampler.backend.clone(),
            resampler.quality,
            resampler.options,
        )
    }

    pub(crate) fn recreates_on_host_rate_change(&self) -> bool {
        self.resampler.is_some()
    }
}

impl<B> Default for AudioDecoderConfig<B> {
    fn default() -> Self {
        Self {
            backend: DecoderBackend::default(),
            gapless_mode: GaplessMode::default(),
            blend_duration: Duration::from_millis(10),
            resampler: None,
        }
    }
}
