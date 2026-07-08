use super::{ReadHeadConfig, resampler::ReadHeadResampler};
use crate::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerSettings,
};

const BACKEND_READ_HEAD: &str = "read-head";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadHeadBackend {
    config: ReadHeadConfig,
}

impl ReadHeadBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config: ReadHeadConfig {
                anti_alias: true,
                interpolation: super::ReadHeadInterpolation::Quadratic,
            },
        }
    }

    #[must_use]
    pub const fn with_config(config: ReadHeadConfig) -> Self {
        Self { config }
    }
}

impl ResamplerBackend for ReadHeadBackend {
    fn build(
        &self,
        settings: &ResamplerSettings,
    ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
        settings.validate(self)?;
        let resampler = ReadHeadResampler::new(self.name(), self.config, settings)?;
        Ok(Box::new(resampler))
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::VARIABLE_RATIO
            | ResamplerCapabilities::RATIO_GLIDE
            | ResamplerCapabilities::REALTIME_SAFE
            | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        BACKEND_READ_HEAD
    }
}
