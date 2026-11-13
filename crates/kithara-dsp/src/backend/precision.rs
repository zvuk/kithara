use std::{num::NonZeroU64, ops::Div};

use fearless_simd::{Level, dispatch, f64x2, prelude::*};
use num_traits::AsPrimitive;

pub(crate) fn ratio_ramp(output: &mut [f64], first: u64, divisor: NonZeroU64) -> usize {
    dispatch!(Level::new(), simd => ratio_kernel(simd, output, first, divisor));
    output.len()
}

pub(super) fn ratio_kernel<S: Simd>(simd: S, output: &mut [f64], first: u64, divisor: NonZeroU64) {
    let divisor = f64x2::splat(simd, divisor.get().as_());
    for (index, chunk) in output.chunks_mut(2).enumerate() {
        let values = f64x2::from_fn(simd, |lane| {
            first
                .saturating_add(index.saturating_mul(2).saturating_add(lane).as_())
                .as_()
        })
        .div(divisor);
        chunk.copy_from_slice(&values.as_slice()[..chunk.len()]);
    }
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
pub(crate) fn narrow_f64(input: &[f64], output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => narrow_kernel(simd, input, output))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
pub(super) fn narrow_kernel<S: Simd>(simd: S, input: &[f64], output: &mut [f32]) -> usize {
    let count = input.len().min(output.len());
    for (input, output) in input[..count].chunks(4).zip(output[..count].chunks_mut(4)) {
        let low = f64x2::from_fn(simd, |lane| input.get(lane).copied().unwrap_or(0.0));
        let high = f64x2::from_fn(simd, |lane| input.get(lane + 2).copied().unwrap_or(0.0));
        let value = low.narrow(high);
        output.copy_from_slice(&value.as_slice()[..output.len()]);
    }
    count
}
