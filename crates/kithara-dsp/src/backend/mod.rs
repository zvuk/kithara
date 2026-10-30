#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) mod accelerate;
#[cfg(feature = "spectrum")]
mod bins;
#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
mod cascade;
mod envelope;
mod fade;
mod fir;
mod interpolate;
#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
pub(crate) mod portable;
mod simd;
#[cfg(all(
    feature = "spectrum",
    any(test, not(any(target_os = "macos", target_os = "ios")))
))]
mod spectrum;
mod strided;
#[cfg(test)]
mod tests;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(crate) use accelerate as platform;
#[cfg(feature = "spectrum")]
pub(crate) use bins::{magnitude, phase};
pub(crate) use envelope::attack_release;
pub(crate) use fade::gains;
pub use fir::Fir;
pub(crate) use interpolate::quadratic;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) use portable as platform;
pub use simd::sanitize;
pub(crate) use simd::{div, mul_add, ramp};
pub(crate) use strided::{gather, scatter};
