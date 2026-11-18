use std::{
    num::NonZeroU64,
    ops::{Add, Div},
};

use fearless_simd::prelude::*;
use num_traits::AsPrimitive;

use crate::backend::route::{route, table};

pub(crate) fn ratio_ramp(output: &mut [f64], first: u64, divisor: NonZeroU64) -> usize {
    route!(table::ratio_ramp => (output, first, divisor))
}

pub(in crate::backend) fn ratio_scalar(
    output: &mut [f64],
    first: u64,
    divisor: NonZeroU64,
) -> usize {
    let divisor: f64 = divisor.get().as_();
    for (index, value) in output.iter_mut().enumerate() {
        let numerator: f64 = first.saturating_add(index.as_()).as_();
        *value = numerator / divisor;
    }
    output.len()
}

#[inline(always)]
pub(in crate::backend) fn ratio_kernel<S: Simd>(
    simd: S,
    output: &mut [f64],
    first: u64,
    divisor: NonZeroU64,
) -> usize {
    let count = output.len();
    let width = S::f64s::LEN;
    let origin = S::u64s::splat(simd, first);
    let divisor = S::f64s::splat(simd, divisor.get().as_());
    let ratio = |offsets| S::f64s::float_from(origin.saturating_add(offsets)).div(divisor);
    let mut offsets: [S::u64s; 4] = std::array::from_fn(|vector| {
        S::u64s::from_fn(simd, |lane| {
            vector.saturating_mul(width).saturating_add(lane).as_()
        })
    });
    let advance = S::u64s::splat(simd, width.saturating_mul(offsets.len()).as_());
    let mut batches = output.chunks_exact_mut(width.saturating_mul(offsets.len()));
    for batch in &mut batches {
        // Independent vectors overlap conversion and division latency.
        let [a, b, c, d] = offsets.map(ratio);
        let (out_a, rest) = batch.split_at_mut(width);
        let (out_b, rest) = rest.split_at_mut(width);
        let (out_c, out_d) = rest.split_at_mut(width);
        a.store_slice(out_a);
        b.store_slice(out_b);
        c.store_slice(out_c);
        d.store_slice(out_d);
        offsets = offsets.map(|offset| offset.add(advance));
    }
    let [mut offset, _, _, _] = offsets;
    let stride = S::u64s::splat(simd, width.as_());
    let mut chunks = batches.into_remainder().chunks_exact_mut(width);
    for chunk in &mut chunks {
        ratio(offset).store_slice(chunk);
        offset = offset.add(stride);
    }
    let tail = chunks.into_remainder();
    if !tail.is_empty() {
        let values = ratio(offset);
        for (slot, value) in tail.iter_mut().zip(values.as_slice()) {
            *slot = *value;
        }
    }
    count
}
