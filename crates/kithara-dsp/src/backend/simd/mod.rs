mod ops;
mod scan;

pub use ops::sanitize;
pub(super) use ops::{blocks, store, update, vector};
pub(crate) use ops::{
    crossfade_linear, div, first_quiet, last_loud, mul_add, ramp, scale_in_place,
};
#[cfg(test)]
pub(super) use ops::{
    crossfade_linear_kernel, div_kernel, mul_add_kernel, ramp_kernel, sanitize_kernel,
    scale_in_place_kernel,
};
pub(super) use scan::{kernel as last_loud_kernel, padded};
