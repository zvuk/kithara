use fearless_simd::{Level, dispatch};

use super::kernel::low_pass_kernel;

pub(crate) fn low_pass<const CHANNELS: usize>(
    coefficient: f32,
    state: &mut [f32; CHANNELS],
    planes: &mut [Option<&mut [f32]>; CHANNELS],
    frames: usize,
) -> usize {
    dispatch!(Level::new(), simd => low_pass_kernel(simd, coefficient, state, planes, frames))
}
