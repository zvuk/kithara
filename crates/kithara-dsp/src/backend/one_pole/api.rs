use fearless_simd::{Level, dispatch};

use super::kernel::low_pass_kernel;

pub(crate) fn low_pass(
    coefficient: f32,
    state: &mut [f32],
    planes: &mut [&mut [f32]],
    frames: usize,
) -> usize {
    dispatch!(Level::new(), simd => low_pass_kernel(simd, coefficient, state, planes, frames))
}
