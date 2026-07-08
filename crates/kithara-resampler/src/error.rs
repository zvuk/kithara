use thiserror::Error;

use crate::ResamplerPlacement;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerBuildError {
    #[error("invalid {resource} sample rate for resampler: {rate}")]
    InvalidSampleRate { resource: &'static str, rate: u32 },

    #[error("invalid resampler options: {detail}")]
    InvalidOptions { detail: &'static str },

    #[error("invalid {resource} ratio for resampler: {ratio}")]
    InvalidRatio { resource: &'static str, ratio: f64 },

    #[error("resampler backend {backend} does not support {mode} mode")]
    UnsupportedMode {
        backend: &'static str,
        mode: &'static str,
    },

    #[error("resampler backend {backend} does not support {placement} placement")]
    UnsupportedPlacement {
        backend: &'static str,
        placement: ResamplerPlacement,
    },

    #[error("resampler backend {backend} construction failed: {detail}")]
    BackendBuild {
        backend: &'static str,
        detail: String,
    },
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerError {
    #[error("invalid resampler buffer: {detail}")]
    InvalidBuffer { detail: &'static str },

    #[error("resampler scratch buffer budget exhausted")]
    BudgetExhausted(#[from] kithara_bufpool::BudgetExhausted),

    #[error("resampler backend failed during {op}: {detail}")]
    Backend { op: &'static str, detail: String },
}
