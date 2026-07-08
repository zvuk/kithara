use bon::Builder;

use crate::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerSettings,
};

const BACKEND_APPLE: &str = "apple-audio-converter";

pub trait AudioConverterFactory: Send + Sync + 'static {
    /// Build a standalone PCM-to-PCM converter for the requested settings.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when the platform converter cannot be
    /// constructed for the requested shape.
    fn build_resampler(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError>;
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AppleAudioConverterBackend<F> {
    config: AppleAudioConverterConfig<F>,
}

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AppleAudioConverterConfig<F> {
    pub factory: F,
}

impl<F> AppleAudioConverterBackend<F> {
    #[must_use]
    pub fn new(factory: F) -> Self {
        Self::with_config(
            AppleAudioConverterConfig::builder()
                .factory(factory)
                .build(),
        )
    }

    #[must_use]
    pub const fn config(&self) -> &AppleAudioConverterConfig<F> {
        &self.config
    }

    #[must_use]
    pub const fn with_config(config: AppleAudioConverterConfig<F>) -> Self {
        Self { config }
    }
}

impl<F> ResamplerBackend for AppleAudioConverterBackend<F>
where
    F: AudioConverterFactory,
{
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        settings.validate(self)?;
        self.config.factory.build_resampler(settings)
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::REPORTS_LATENCY
            | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        BACKEND_APPLE
    }
}
