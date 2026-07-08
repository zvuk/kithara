#![deny(unsafe_code)]

//! Sample-rate resampler contracts and backend adapters.

mod backend;
mod capabilities;
mod config;
mod error;
mod factory;
mod mode;
mod placement;
#[cfg(feature = "resample-readhead")]
pub mod read_head;
#[cfg(feature = "resample-rubato")]
pub mod rubato;
mod traits;

pub use backend::ResamplerBackend;
pub use capabilities::ResamplerCapabilities;
pub use config::{
    RatioGlide, ResamplerBackendConfig, ResamplerConfig, ResamplerOptions, ResamplerQuality,
    ResamplerSettings,
};
pub use error::{ResamplerBuildError, ResamplerError};
pub use factory::create_resampler;
pub use mode::ResamplerMode;
pub use placement::ResamplerPlacement;
pub use traits::{Resampler, ResamplerControl, ResamplerProcess};
