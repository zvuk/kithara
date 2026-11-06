mod cascade;
mod interpolate;
#[cfg(feature = "spectrum")]
mod spectrum;

pub(crate) use kithara_apple::accelerate::{
    add_f32 as add, affine_f32 as affine, deinterleave_pair_f32 as deinterleave_pair,
    downmix_pair_f32 as downmix_pair, interleave_pair_f32 as interleave_pair,
    max_magnitude_f32 as peak, multiply_in_place_f32 as multiply_in_place,
    scale_add_f32 as scale_add, sub_f32 as sub, sum_squares_f32 as sum_squares,
};

#[cfg(feature = "spectrum")]
pub(crate) use self::spectrum::{Dft, Work, correlate, multiply};
pub(crate) use self::{cascade::Cascade, interpolate::interpolate};
