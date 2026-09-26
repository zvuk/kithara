use kithara_bufpool::HasPool;

use super::resampler::AppleResampler;
use crate::{
    ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode, ResamplerSettings,
};

mod consts {
    pub(super) const BACKEND_APPLE: &str = "apple-audio-converter";
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AppleAudioConverterBackend;

impl AppleAudioConverterBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ResamplerBackend for AppleAudioConverterBackend {
    type Resampler = AppleResampler;

    fn build<S>(
        &self,
        settings: &ResamplerSettings<S>,
    ) -> Result<Self::Resampler, ResamplerBuildError>
    where
        S: HasPool<f32>,
    {
        settings.validate(self)?;
        let ResamplerMode::FixedRatio {
            source_sample_rate,
            target_sample_rate,
        } = settings.mode
        else {
            return Err(ResamplerBuildError::UnsupportedMode {
                backend: consts::BACKEND_APPLE,
                mode: settings.mode.label(),
            });
        };

        AppleResampler::new(
            source_sample_rate.get(),
            target_sample_rate.get(),
            settings.channels.get(),
            settings.options.chunk_size,
            &settings.pools,
        )
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::REPORTS_LATENCY
            | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        consts::BACKEND_APPLE
    }
}
