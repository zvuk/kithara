use fearless_simd::{Level, dispatch};

use super::kernel::low_pass_kernel;

/// Filters equal-length prepared views, with one state slot per plane.
pub(crate) fn low_pass(coefficient: f32, state: &mut [f32], planes: &mut [&mut [f32]]) -> usize {
    dispatch!(Level::new(), simd => low_pass_kernel(simd, coefficient, state, planes))
}
