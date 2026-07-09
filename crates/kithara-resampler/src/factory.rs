use crate::{ResamplerBackend, ResamplerBuildError, ResamplerConfig};

/// Build the selected standalone resampler backend.
///
/// # Errors
///
/// Returns [`ResamplerBuildError`] when config validation fails or when the
/// selected backend fails to construct the processor.
pub fn create_resampler<B>(config: &ResamplerConfig<B>) -> Result<B::Resampler, ResamplerBuildError>
where
    B: ResamplerBackend,
{
    config.validate()?;
    config.backend.build(&config.settings)
}
