mod error;
#[cfg(test)]
mod frame_tests;
mod frames;
mod interpolation;
mod ramp;
#[cfg(test)]
mod tests;

pub use error::InterpError;
pub use frames::{interpolate_frame, weighted_frames};
pub use interpolation::{Interpolation, interpolate, quadratic, sinc_hann_weight};
pub use ramp::RateRamp;
