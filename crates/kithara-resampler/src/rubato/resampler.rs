use std::num::{NonZeroU32, NonZeroUsize};

use num_traits::cast::ToPrimitive;

use super::{RubatoBackend, RubatoConfig, engine::RubatoEngine};
use crate::{
    Resampler, ResamplerBackend, ResamplerBuildError, ResamplerCapabilities, ResamplerError,
    ResamplerMode, ResamplerProcess, ResamplerSettings,
};

pub struct RubatoResampler {
    channels: NonZeroUsize,
    mode: ResamplerMode,
    config: RubatoConfig,
    inner: RubatoEngine,
}

impl RubatoResampler {
    pub(super) fn new(
        backend: &'static str,
        config: RubatoConfig,
        settings: &ResamplerSettings,
    ) -> Result<Self, ResamplerBuildError> {
        let ResamplerMode::FixedRatio {
            source_sample_rate,
            target_sample_rate,
        } = settings.mode
        else {
            return Err(ResamplerBuildError::UnsupportedMode {
                backend,
                mode: settings.mode.label(),
            });
        };
        let source_rate = source_sample_rate.get();
        let target_rate = target_sample_rate.get();
        let inner = RubatoEngine::new(
            config,
            settings.quality,
            source_rate,
            target_rate,
            settings.channels,
            settings.options,
            settings.pcm_pool.clone(),
        )
        .map_err(|err| ResamplerBuildError::BackendBuild {
            backend,
            detail: err.to_string(),
        })?;
        let mode = ResamplerMode::FixedRatio {
            source_sample_rate: non_zero_rate("source", source_rate)?,
            target_sample_rate: non_zero_rate("target", target_rate)?,
        };
        Ok(Self {
            config,
            inner,
            mode,
            channels: settings.channels,
        })
    }

    fn ratio(&self) -> f64 {
        self.inner.resample_ratio()
    }
}

impl Resampler for RubatoResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        RubatoBackend::with_config(self.config).capabilities()
    }

    fn channels(&self) -> NonZeroUsize {
        self.channels
    }

    delegate::delegate! {
        to self.inner {
            fn input_frames_max(&self) -> usize;
            fn input_frames_next(&self) -> usize;
            fn output_delay(&self) -> usize;
            fn output_frames_max(&self) -> usize;
            fn output_frames_next(&self) -> usize;
            fn reset(&mut self);
        }
    }
    fn mode(&self) -> ResamplerMode {
        self.mode
    }

    fn output_frames_for_input(&self, input_frames: usize) -> usize {
        let Some(input_frames) = input_frames.to_f64() else {
            return usize::MAX;
        };
        let frames = (input_frames * self.ratio()).ceil();
        if !frames.is_finite() || frames <= 0.0 {
            return 0;
        }

        frames.to_usize().unwrap_or(usize::MAX)
    }

    fn process_into_buffer(
        &mut self,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
    ) -> Result<ResamplerProcess, ResamplerError> {
        self.inner
            .process_into_buffer(input, output)
            .map_err(|err| ResamplerError::Backend {
                detail: err.to_string(),
                op: "rubato process",
            })
    }
}

fn non_zero_rate(
    resource: &'static str,
    sample_rate: u32,
) -> Result<NonZeroU32, ResamplerBuildError> {
    NonZeroU32::new(sample_rate).ok_or(ResamplerBuildError::InvalidSampleRate {
        resource,
        rate: sample_rate,
    })
}
