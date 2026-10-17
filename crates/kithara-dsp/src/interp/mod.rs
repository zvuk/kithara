mod error;
mod interpolation;
mod ramp;
#[cfg(test)]
mod tests;

pub use error::InterpError;
pub use interpolation::{Interpolation, interpolate, quadratic, sinc_hann_weight};
pub use ramp::RateRamp;
