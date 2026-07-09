use crate::{Resampler, ResamplerBuildError, ResamplerCapabilities, ResamplerSettings};

pub trait ResamplerBackend: Clone + Send + Sync + 'static {
    type Resampler: Resampler;

    /// Build a standalone resampler for the supplied settings.
    ///
    /// # Errors
    ///
    /// Returns [`ResamplerBuildError`] when the settings are invalid for this
    /// backend or backend construction fails.
    fn build(&self, settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError>;

    fn capabilities(&self) -> ResamplerCapabilities;

    fn name(&self) -> &'static str;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoResamplerBackend;

pub struct NoResampler;

impl ResamplerBackend for NoResamplerBackend {
    type Resampler = NoResampler;

    fn build(&self, _settings: &ResamplerSettings) -> Result<Self::Resampler, ResamplerBuildError> {
        Err(ResamplerBuildError::BackendBuild {
            backend: self.name(),
            detail: "no resampler backend compiled".to_owned(),
        })
    }

    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::empty()
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

impl Resampler for NoResampler {
    fn capabilities(&self) -> ResamplerCapabilities {
        ResamplerCapabilities::empty()
    }

    fn channels(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::MIN
    }

    fn input_frames_max(&self) -> usize {
        0
    }

    fn input_frames_next(&self) -> usize {
        0
    }

    fn mode(&self) -> crate::ResamplerMode {
        crate::ResamplerMode::FixedRatio {
            source_sample_rate: std::num::NonZeroU32::MIN,
            target_sample_rate: std::num::NonZeroU32::MIN,
        }
    }

    fn output_frames_for_input(&self, _input_frames: usize) -> usize {
        0
    }

    fn output_frames_max(&self) -> usize {
        0
    }

    fn output_frames_next(&self) -> usize {
        0
    }

    fn process_into_buffer(
        &mut self,
        _input: &[&[f32]],
        _output: &mut [&mut [f32]],
    ) -> Result<crate::ResamplerProcess, crate::ResamplerError> {
        Err(crate::ResamplerError::Backend {
            op: "process",
            detail: "no resampler backend compiled".to_owned(),
        })
    }

    fn reset(&mut self) {}
}
