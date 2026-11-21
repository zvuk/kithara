mod ops;
mod scan;

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
pub(crate) use ops::narrow_f64;
pub use ops::sanitize;
pub(super) use ops::{blocks, div_kernel, mul_add_kernel, ramp_kernel, store, update, vector};
pub(crate) use ops::{
    crossfade_linear, div, first_quiet, last_loud, mul_add, multiply_squared_in_place, ramp,
    scale_in_place,
};
#[cfg(test)]
pub(super) use ops::{crossfade_linear_kernel, sanitize_kernel, scale_in_place_kernel};
#[cfg(test)]
pub(super) use ops::{multiply_squared_kernel, narrow_kernel};
pub(super) use scan::{kernel as last_loud_kernel, padded};
