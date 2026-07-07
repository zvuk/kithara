use thiserror::Error;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerBuildError {
    #[error("invalid {resource} sample rate for resampler: {rate}")]
    InvalidSampleRate { resource: &'static str, rate: u32 },

    #[cfg(feature = "resample-rubato")]
    #[error("rubato resampler construction failed: {0}")]
    Rubato(#[from] rubato::ResamplerConstructionError),
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerError {
    #[cfg(feature = "resample-rubato")]
    #[error("rubato resampler failed: {0}")]
    Rubato(#[from] rubato::ResampleError),
}
