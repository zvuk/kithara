use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
};

use bon::Builder;
use kithara_bufpool::PoolRegion;
use serde::Deserialize;

use crate::{ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerMode};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
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
    pub frames: NonZeroU32,
    pub target_ratio: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Builder, Deserialize)]
#[builder(const, state_mod(vis = "pub"))]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ResamplerOptions {
    #[builder(default = 8.0)]
    pub max_ratio_adjustment: f64,
    #[builder(default = 0.0001)]
    pub passthrough_tolerance: f64,
    #[builder(default = 4_096)]
    pub chunk_size: usize,
}

impl Default for ResamplerOptions {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerSettings<S> {
    pub channels: NonZeroUsize,
    pub pools: PoolRegion<S>,
    pub mode: ResamplerMode,
    #[builder(default)]
    pub options: ResamplerOptions,
    #[builder(default)]
    pub quality: ResamplerQuality,
}

impl<S> Clone for ResamplerSettings<S> {
    fn clone(&self) -> Self {
        Self {
            channels: self.channels,
            pools: self.pools.clone(),
            mode: self.mode,
            options: self.options,
            quality: self.quality,
        }
    }
}

impl<S> fmt::Debug for ResamplerSettings<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResamplerSettings")
            .field("channels", &self.channels)
            .field("pools", &self.pools)
            .field("mode", &self.mode)
            .field("options", &self.options)
            .field("quality", &self.quality)
            .finish_non_exhaustive()
    }
}

impl<S> ResamplerSettings<S> {
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

#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct ResamplerConfig<B, S> {
    pub backend: B,
    pub settings: ResamplerSettings<S>,
}

impl<B, S> Clone for ResamplerConfig<B, S>
where
    B: Clone,
{
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            settings: self.settings.clone(),
        }
    }
}

impl<B, S> fmt::Debug for ResamplerConfig<B, S>
where
    B: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResamplerConfig")
            .field("backend", &self.backend)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl<B, S> ResamplerConfig<B, S>
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

pub(crate) fn validate_settings<S>(
    backend: &'static str,
    capabilities: ResamplerCapabilities,
    settings: &ResamplerSettings<S>,
) -> Result<(), ResamplerBuildError> {
    validate_options(settings.options)?;
    validate_mode(backend, capabilities, settings.mode)?;
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

    use kithara_bufpool::HasPool;
    use kithara_test_utils::kithara;

    use crate::{
        ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerConfig,
        ResamplerMode, ResamplerOptions, ResamplerSettings,
        backend::NoResampler,
        test_pools::{TestPools, pools},
    };

    #[derive(Clone, Debug)]
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
        type Resampler = NoResampler;

        fn build<S>(
            &self,
            _settings: &ResamplerSettings<S>,
        ) -> Result<Self::Resampler, ResamplerBuildError>
        where
            S: HasPool<f32>,
        {
            Err(ResamplerBuildError::BackendBuild {
                backend: self.name(),
                detail: "test backend has no processor".into(),
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
        NonZeroU32::new(value).expect("sample rate must be non-zero")
    }

    fn stereo() -> NonZeroUsize {
        NonZeroUsize::new(2).expect("channel count must be non-zero")
    }

    #[kithara::test(native, flash(false))]
    fn defaults_match_current_playback_values() {
        let options = ResamplerOptions::default();

        assert_eq!(options.chunk_size, 4_096);
        assert_eq!(options.passthrough_tolerance, 0.0001);
        assert_eq!(options.max_ratio_adjustment, 8.0);
    }

    #[kithara::test(native, flash(false))]
    fn builder_overrides_single_tunable_without_losing_defaults() {
        let options = ResamplerOptions::builder().chunk_size(1_024).build();

        assert_eq!(options.chunk_size, 1_024);
        assert_eq!(options.passthrough_tolerance, 0.0001);
        assert_eq!(options.max_ratio_adjustment, 8.0);
    }

    #[kithara::test(native, flash(false))]
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
            .pools(pools())
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

    #[kithara::test(native, flash(false))]
    fn standalone_config_uses_injected_pool() {
        let pools = pools();
        let settings = ResamplerSettings::builder()
            .channels(stereo())
            .mode(ResamplerMode::FixedRatio {
                source_sample_rate: sample_rate(44_100),
                target_sample_rate: sample_rate(48_000),
            })
            .pools(pools.clone())
            .build();
        let config = ResamplerConfig::builder()
            .backend(TestBackend::fixed())
            .settings(settings)
            .build();

        let _: &ResamplerSettings<TestPools> = &config.settings;
        assert_eq!(config.settings.pools.stats(), pools.stats());
        assert_eq!(config.clone().settings.pools.stats(), pools.stats());
        assert!(format!("{config:?}").contains("ResamplerConfig"));
        assert!(config.validate().is_ok());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod document_tests {
    use kithara_test_utils::kithara;

    use crate::{ResamplerOptions, ResamplerQuality};

    #[kithara::test(native, flash(false))]
    fn a_quality_document_key_types_to_the_named_variant() {
        let quality: ResamplerQuality =
            serde_yaml_ng::from_str("high\n").expect("the document types");

        assert_eq!(quality, ResamplerQuality::High);
    }

    #[kithara::test(native, flash(false))]
    fn a_partial_options_document_leaves_the_rest_at_default() {
        let options: ResamplerOptions =
            serde_yaml_ng::from_str("chunk_size: 1024\n").expect("the document types");

        assert_eq!(options.chunk_size, 1_024);
        assert!(
            (options.max_ratio_adjustment - 8.0).abs() < f64::EPSILON,
            "an unnamed parameter keeps its default"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_unknown_options_field_is_rejected_and_named() {
        let error = serde_yaml_ng::from_str::<ResamplerOptions>("resample_ratio: 2.0\n")
            .expect_err("a typo must not be silently ignored");

        assert!(error.to_string().contains("resample_ratio"), "{error}");
    }
}
