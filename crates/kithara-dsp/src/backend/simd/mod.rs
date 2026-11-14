mod api;
mod block;

pub use api::sanitize;
pub(super) use api::{blocks, update};
pub(crate) use api::{crossfade_linear, div, first_quiet, mul_add, ramp, scale_in_place};
#[cfg(test)]
pub(super) use api::{
    crossfade_linear_kernel, div_kernel, mul_add_kernel, ramp_kernel, sanitize_kernel,
    scale_in_place_kernel,
};
pub(super) use block::{padded, store, vector};
