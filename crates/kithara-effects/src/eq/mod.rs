mod band;
mod config;
mod effect;
mod isolator;
mod stereo;

pub use band::{EqBandConfig, FilterKind, generate_log_spaced_bands};
pub use config::EqConfig;
pub use effect::EqEffect;
pub use isolator::IsolatorEq;
pub use stereo::{EqLayout, StereoEq};

#[cfg(test)]
mod tests;
