use thiserror::Error;

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use crate::apple::{consts::os_status_to_string, ffi::OSStatus};

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerBuildError {
    #[error("invalid {resource} sample rate for resampler: {rate}")]
    InvalidSampleRate { resource: &'static str, rate: u32 },

    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    #[error("apple resampler construction failed during {op}: {status_message}")]
    Apple {
        op: &'static str,
        status: i32,
        status_message: String,
    },

    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    #[error("invalid apple resampler config: {detail}")]
    AppleConfig { detail: &'static str },

    #[cfg(feature = "resample-rubato")]
    #[error("rubato resampler construction failed: {0}")]
    Rubato(#[from] rubato::ResamplerConstructionError),
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
impl ResamplerBuildError {
    pub(crate) fn apple_config(detail: &'static str) -> Self {
        Self::AppleConfig { detail }
    }

    pub(crate) fn apple_status(op: &'static str, status: OSStatus) -> Self {
        Self::Apple {
            op,
            status,
            status_message: os_status_to_string(status),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ResamplerError {
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    #[error("apple resampler failed during {op}: {status_message}")]
    Apple {
        op: &'static str,
        status: i32,
        status_message: String,
    },

    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    #[error("invalid apple resampler buffer: {detail}")]
    AppleBuffer { detail: &'static str },

    #[cfg(feature = "resample-rubato")]
    #[error("rubato resampler failed: {0}")]
    Rubato(#[from] rubato::ResampleError),
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
impl ResamplerError {
    pub(crate) fn apple_buffer(detail: &'static str) -> Self {
        Self::AppleBuffer { detail }
    }

    pub(crate) fn apple_status(op: &'static str, status: OSStatus) -> Self {
        Self::Apple {
            op,
            status,
            status_message: os_status_to_string(status),
        }
    }
}
