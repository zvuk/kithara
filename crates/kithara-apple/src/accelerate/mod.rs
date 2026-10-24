mod biquad;
mod dft;
mod ffi;
mod interpolation;
mod layout;
#[cfg(test)]
mod tests;
mod vector;

pub use biquad::{BiquadError, MultichannelBiquad};
pub use dft::{DftError, RealDft};
pub use interpolation::{OutOfWindow, linear_interpolate_f32, quadratic_interpolate_f32};
pub use layout::{deinterleave_pair_f32, downmix_pair_f32, interleave_pair_f32};
pub use vector::{
    add_f32, affine_f32, correlate_f32, max_magnitude_f32, multiply_f32, scale_add_f32, sub_f32,
    sum_squares_f32,
};
