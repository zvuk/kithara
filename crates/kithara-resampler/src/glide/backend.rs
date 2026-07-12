use super::{GlideConfig, resampler::GlideResampler};
use crate::{ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerSettings};

const BACKEND_GLIDE: &str = "glide";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GlideBackend {
    config: GlideConfig,
}

impl GlideBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config: GlideConfig {
                anti_alias: true,
                interpolation: super::GlideInterpolation::Quadratic,
            },
        }
    }

    #[must_use]
    pub const fn with_config(config: GlideConfig) -> Self {
        Self { config }
    }
}

impl ResamplerBackend for GlideBackend {
    type Resampler = GlideResampler;

    fn build(&self, settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError> {
        settings.validate(self)?;
        GlideResampler::new(self.name(), self.config, settings)
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::FIXED_RATIO
            | ResamplerCapabilities::VARIABLE_RATIO
            | ResamplerCapabilities::RATIO_GLIDE
            | ResamplerCapabilities::REALTIME_SAFE
            | ResamplerCapabilities::STANDALONE
    }

    fn name(&self) -> &'static str {
        BACKEND_GLIDE
    }
}
