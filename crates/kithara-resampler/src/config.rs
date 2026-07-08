use std::num::{NonZeroU32, NonZeroUsize};

use bon::Builder;
use kithara_bufpool::PcmPool;

use crate::{
    ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode, ResamplerPlacement,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResamplerQuality {
    Fast,
    Normal,
    Good,
    #[default]
    High,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct RatioGlide {
    pub target_ratio: f64,
    pub frames: NonZeroU32,
}

#[derive(Clone, Copy, Debug, PartialEq, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerOptions {
    #[builder(default = 4_096)]
    pub chunk_size: usize,
    #[builder(default = 0.0001)]
    pub passthrough_tolerance: f64,
    #[builder(default = 8.0)]
    pub max_ratio_adjustment: f64,
}

impl Default for ResamplerOptions {
    fn default() -> Self {
        Self {
            chunk_size: 4_096,
            passthrough_tolerance: 0.0001,
            max_ratio_adjustment: 8.0,
        }
    }
}

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerSettings {
    pub channels: NonZeroUsize,
    pub mode: ResamplerMode,
    #[builder(default = ResamplerPlacement::Standalone)]
    pub placement: ResamplerPlacement,
    #[builder(default)]
    pub quality: ResamplerQuality,
    #[builder(default)]
    pub options: ResamplerOptions,
    pub pcm_pool: PcmPool,
}

impl ResamplerSettings {
    /// Validate the config without constructing a backend.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when the selected backend does not
    /// support the requested placement or mode, or when numeric tunables are
    /// invalid.
    pub fn validate<B>(&self, backend: &B) -> Result<(), ResamplerBuildError>
    where
        B: ResamplerBackend,
    {
        validate_settings(backend.name(), backend.capabilities(), self)
    }
}

#[derive(Clone, Debug, Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerConfig<B> {
    pub backend: B,
    pub settings: ResamplerSettings,
}

impl<B> ResamplerConfig<B>
where
    B: ResamplerBackend,
{
    /// Validate the configured backend and settings without constructing it.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when the backend does not support the
    /// requested placement or mode, or when numeric tunables are invalid.
    pub fn validate(&self) -> Result<(), ResamplerBuildError> {
        self.settings.validate(&self.backend)
    }
}

pub(crate) fn validate_settings(
    backend: &'static str,
    capabilities: ResamplerCapabilities,
    settings: &ResamplerSettings,
) -> Result<(), ResamplerBuildError> {
    validate_options(settings.options)?;
    validate_mode(backend, capabilities, settings.mode)?;

    if settings.placement == ResamplerPlacement::Standalone
        && !capabilities.contains(ResamplerCapabilities::STANDALONE)
    {
        return Err(ResamplerBuildError::UnsupportedPlacement {
            backend,
            placement: settings.placement,
        });
    }

    Ok(())
}

fn validate_options(options: ResamplerOptions) -> Result<(), ResamplerBuildError> {
    if options.chunk_size == 0 {
        return Err(ResamplerBuildError::InvalidOptions {
            detail: "chunk_size must be greater than zero",
        });
    }
    if !options.passthrough_tolerance.is_finite() || options.passthrough_tolerance < 0.0 {
        return Err(ResamplerBuildError::InvalidOptions {
            detail: "passthrough_tolerance must be finite and non-negative",
        });
    }
    if !options.max_ratio_adjustment.is_finite() || options.max_ratio_adjustment <= 0.0 {
        return Err(ResamplerBuildError::InvalidOptions {
            detail: "max_ratio_adjustment must be finite and positive",
        });
    }

    Ok(())
}

fn validate_mode(
    backend: &'static str,
    capabilities: ResamplerCapabilities,
    mode: ResamplerMode,
) -> Result<(), ResamplerBuildError> {
    match mode {
        ResamplerMode::FixedRatio { .. } => {
            if !capabilities.contains(ResamplerCapabilities::FIXED_RATIO) {
                return Err(ResamplerBuildError::UnsupportedMode {
                    backend,
                    mode: mode.label(),
                });
            }
        }
        ResamplerMode::VariableRatio {
            initial_ratio,
            glide,
            ..
        } => {
            if !capabilities.contains(ResamplerCapabilities::VARIABLE_RATIO) {
                return Err(ResamplerBuildError::UnsupportedMode {
                    backend,
                    mode: mode.label(),
                });
            }
            validate_ratio("initial_ratio", initial_ratio)?;
            if let Some(glide) = glide {
                if !capabilities.contains(ResamplerCapabilities::RATIO_GLIDE) {
                    return Err(ResamplerBuildError::UnsupportedMode {
                        backend,
                        mode: "ratio-glide",
                    });
                }
                validate_ratio("glide target_ratio", glide.target_ratio)?;
            }
        }
    }

    Ok(())
}

fn validate_ratio(resource: &'static str, ratio: f64) -> Result<(), ResamplerBuildError> {
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(ResamplerBuildError::InvalidRatio { resource, ratio });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use kithara_bufpool::PcmPool;

    use crate::{
        Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerConfig,
        ResamplerMode, ResamplerOptions, ResamplerSettings,
    };

    struct TestBackend {
        capabilities: ResamplerCapabilities,
    }

    impl TestBackend {
        fn fixed() -> Self {
            Self {
                capabilities: ResamplerCapabilities::FIXED_RATIO
                    | ResamplerCapabilities::STANDALONE,
            }
        }
    }

    impl ResamplerBackend for TestBackend {
        fn build(
            &self,
            _settings: &ResamplerSettings,
        ) -> Result<Box<dyn Resampler>, ResamplerBuildError> {
            Err(ResamplerBuildError::BackendBuild {
                backend: self.name(),
                detail: "test backend has no processor",
            })
        }

        fn capabilities(&self) -> ResamplerCapabilities {
            self.capabilities
        }

        fn name(&self) -> &'static str {
            "test"
        }
    }

    fn sample_rate(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap_or_else(|| panic!("sample rate must be non-zero"))
    }

    fn stereo() -> NonZeroUsize {
        NonZeroUsize::new(2).unwrap_or_else(|| panic!("channel count must be non-zero"))
    }

    #[test]
    fn defaults_match_current_playback_values() {
        let options = ResamplerOptions::default();

        assert_eq!(options.chunk_size, 4_096);
        assert_eq!(options.passthrough_tolerance, 0.0001);
        assert_eq!(options.max_ratio_adjustment, 8.0);
    }

    #[test]
    fn builder_overrides_single_tunable_without_losing_defaults() {
        let options = ResamplerOptions::builder().chunk_size(1_024).build();

        assert_eq!(options.chunk_size, 1_024);
        assert_eq!(options.passthrough_tolerance, 0.0001);
        assert_eq!(options.max_ratio_adjustment, 8.0);
    }

    #[test]
    fn config_requires_positive_chunk_size() {
        let settings = ResamplerSettings::builder()
            .channels(stereo())
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: sample_rate(44_100),
                target_sample_rate: sample_rate(48_000),
            })
            .options(ResamplerOptions {
                chunk_size: 0,
                ..ResamplerOptions::default()
            })
            .pcm_pool(PcmPool::new(4, 4_096))
            .build();
        let config = ResamplerConfig::builder()
            .backend(TestBackend::fixed())
            .settings(settings)
            .build();

        assert!(matches!(
            config.validate(),
            Err(ResamplerBuildError::InvalidOptions { .. })
        ));
    }

    #[test]
    fn standalone_config_uses_injected_pool() {
        let pool = PcmPool::new(4, 4_096);
        let settings = ResamplerSettings::builder()
            .channels(stereo())
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: sample_rate(44_100),
                target_sample_rate: sample_rate(48_000),
            })
            .pcm_pool(pool.clone())
            .build();
        let config = ResamplerConfig::builder()
            .backend(TestBackend::fixed())
            .settings(settings)
            .build();

        assert_eq!(config.settings.pcm_pool.stats(), pool.stats());
        assert!(config.validate().is_ok());
    }
}
