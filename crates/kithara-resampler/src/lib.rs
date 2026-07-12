#![deny(unsafe_code)]

//! Sample-rate resampler contracts and backend adapters.

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;
mod backend;
mod capabilities;
mod config;
mod error;
mod factory;
#[cfg(feature = "resample-glide")]
pub mod glide;
mod mode;
mod mono;
#[cfg(feature = "resample-rubato")]
pub mod rubato;
mod traits;

pub use backend::{NoResamplerBackend, ResamplerBackend};
pub use capabilities::ResamplerCapabilities;
pub use config::{
    Decode, RatioGlide, Resample, ResamplerConfig, ResamplerOptions, ResamplerQuality,
    ResamplerSettings, Unit,
};
pub use error::{ResamplerBuildError, ResamplerError};
pub use factory::create_resampler;
pub use mode::ResamplerMode;
pub use mono::{MonoStream, MonoStreamConfig};
pub use traits::{Resampler, ResamplerControl, ResamplerProcess};
