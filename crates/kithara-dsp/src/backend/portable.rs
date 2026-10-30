use std::ops::{Add, Mul, Sub};

use fearless_simd::{Level, dispatch, prelude::*};

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) use super::cascade::Cascade;
pub(crate) use super::interpolate::interpolate;
#[cfg(feature = "spectrum")]
use super::simd::zip_map;
use super::simd::{padded, update};
#[cfg(all(feature = "spectrum", not(any(target_os = "macos", target_os = "ios"))))]
pub(crate) use super::spectrum::{Dft, Work, correlate};

mod consts {
    /// Scale that turns the sum of a pair into its mean.
    pub(super) const PAIR_MEAN: f32 = 0.5;
}

pub(crate) fn deinterleave_pair(input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => deinterleave_pair_kernel(simd, input, left, right))
}

pub(crate) fn interleave_pair(left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => interleave_pair_kernel(simd, left, right, output))
}

#[inline(always)]
pub(super) fn interleave_pair_kernel<S: Simd>(
    simd: S,
    left: &[f32],
    right: &[f32],
    output: &mut [f32],
) -> usize {
    let frames = left.len().min(right.len()).min(output.len() / 2);
    let (Some(left), Some(right), Some(pairs)) = (
        left.get(..frames),
        right.get(..frames),
        output.as_chunks_mut::<2>().0.get_mut(..frames),
    ) else {
        return 0;
    };
    let lanes = S::f32s::LEN;
    let mut lefts = left.chunks_exact(lanes);
    let mut rights = right.chunks_exact(lanes);
    let mut outputs = pairs.chunks_exact_mut(lanes);
    for ((l, r), out) in (&mut lefts).zip(&mut rights).zip(&mut outputs) {
        let (lo, hi) = S::f32s::from_slice(simd, l).interleave(S::f32s::from_slice(simd, r));
        let (out_lo, out_hi) = out.as_flattened_mut().split_at_mut(lanes);
        lo.store_slice(out_lo);
        hi.store_slice(out_hi);
    }
    let (lo, hi) = padded(simd, lefts.remainder()).interleave(padded(simd, rights.remainder()));
    for (slot, sample) in outputs
        .into_remainder()
        .as_flattened_mut()
        .iter_mut()
        .zip(lo.as_slice().iter().chain(hi.as_slice()))
    {
        *slot = *sample;
    }
    frames
}

#[inline(always)]
pub(super) fn deinterleave_pair_kernel<S: Simd>(
    simd: S,
    input: &[f32],
    left: &mut [f32],
    right: &mut [f32],
) -> usize {
    let frames = (input.len() / 2).min(left.len()).min(right.len());
    let (Some(pairs), Some(left), Some(right)) = (
        input.as_chunks::<2>().0.get(..frames),
        left.get_mut(..frames),
        right.get_mut(..frames),
    ) else {
        return 0;
    };
    let lanes = S::f32s::LEN;
    let mut inputs = pairs.chunks_exact(lanes);
    let mut lefts = left.chunks_exact_mut(lanes);
    let mut rights = right.chunks_exact_mut(lanes);
    for ((block, l), r) in (&mut inputs).zip(&mut lefts).zip(&mut rights) {
        let (lo, hi) = block.as_flattened().split_at(lanes);
        let (even, odd) = S::f32s::from_slice(simd, lo).deinterleave(S::f32s::from_slice(simd, hi));
        even.store_slice(l);
        odd.store_slice(r);
    }
    let tail = inputs.remainder().as_flattened();
    let (lo, hi) = tail.split_at(tail.len().min(lanes));
    let (even, odd) = padded(simd, lo).deinterleave(padded(simd, hi));
    for (slot, sample) in lefts.into_remainder().iter_mut().zip(even.as_slice()) {
        *slot = *sample;
    }
    for (slot, sample) in rights.into_remainder().iter_mut().zip(odd.as_slice()) {
        *slot = *sample;
    }
    frames
}

pub(crate) fn downmix_pair(input: &[f32], mono: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => downmix_pair_kernel(simd, input, mono))
}

#[inline(always)]
pub(super) fn downmix_pair_kernel<S: Simd>(simd: S, input: &[f32], mono: &mut [f32]) -> usize {
    let frames = (input.len() / 2).min(mono.len());
    let (Some(pairs), Some(mono)) = (
        input.as_chunks::<2>().0.get(..frames),
        mono.get_mut(..frames),
    ) else {
        return 0;
    };
    let lanes = S::f32s::LEN;
    let half = S::f32s::splat(simd, consts::PAIR_MEAN);
    let mean = |lo: S::f32s, hi: S::f32s| {
        let (left, right) = lo.deinterleave(hi);
        left.add(right).mul(half)
    };
    let mut inputs = pairs.chunks_exact(lanes);
    let mut outputs = mono.chunks_exact_mut(lanes);
    for (block, out) in (&mut inputs).zip(&mut outputs) {
        let (lo, hi) = block.as_flattened().split_at(lanes);
        mean(S::f32s::from_slice(simd, lo), S::f32s::from_slice(simd, hi)).store_slice(out);
    }
    let tail = inputs.remainder().as_flattened();
    let (lo, hi) = tail.split_at(tail.len().min(lanes));
    let tail = mean(padded(simd, lo), padded(simd, hi));
    for (slot, value) in outputs.into_remainder().iter_mut().zip(tail.as_slice()) {
        *slot = *value;
    }
    frames
}

#[cfg(feature = "spectrum")]
pub(crate) fn multiply(a: &[f32], b: &[f32], output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => multiply_kernel(simd, a, b, output))
}

#[cfg(feature = "spectrum")]
#[inline(always)]
pub(super) fn multiply_kernel<S: Simd>(simd: S, a: &[f32], b: &[f32], output: &mut [f32]) -> usize {
    zip_map(simd, [a, b], output, Mul::mul)
}

/// `Σ a[i]·b[i]` over the common prefix: one multiply-add per lane, then
/// the lanes summed.
#[inline(always)]
pub(super) fn dot<S: Simd>(simd: S, a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let (Some(a), Some(b)) = (a.get(..len), b.get(..len)) else {
        return 0.0;
    };
    let mut lefts = a.chunks_exact(S::f32s::LEN);
    let mut rights = b.chunks_exact(S::f32s::LEN);
    let mut sum = S::f32s::splat(simd, 0.0);
    for (x, y) in (&mut lefts).zip(&mut rights) {
        sum = S::f32s::from_slice(simd, x).mul_add(S::f32s::from_slice(simd, y), sum);
    }
    padded(simd, lefts.remainder())
        .mul_add(padded(simd, rights.remainder()), sum)
        .reduce_sum()
}

pub(crate) fn sum_squares(samples: &[f32]) -> f32 {
    dispatch!(Level::new(), simd => sum_squares_kernel(simd, samples))
}

#[inline(always)]
pub(super) fn sum_squares_kernel<S: Simd>(simd: S, samples: &[f32]) -> f32 {
    dot(simd, samples, samples)
}

pub(crate) fn affine(samples: &mut [f32], scale: f32, offset: f32) -> usize {
    dispatch!(Level::new(), simd => affine_kernel(simd, samples, scale, offset))
}

#[inline(always)]
pub(super) fn affine_kernel<S: Simd>(
    simd: S,
    samples: &mut [f32],
    scale: f32,
    offset: f32,
) -> usize {
    let (scale, offset) = (S::f32s::splat(simd, scale), S::f32s::splat(simd, offset));
    update(simd, samples, [], |x, []| x.mul_add_precise(scale, offset))
}

pub(crate) fn sub(x: &mut [f32], y: &[f32]) -> usize {
    dispatch!(Level::new(), simd => sub_kernel(simd, x, y))
}

#[inline(always)]
pub(super) fn sub_kernel<S: Simd>(simd: S, x: &mut [f32], y: &[f32]) -> usize {
    update(simd, x, [y], |x, [y]| x.sub(y))
}

pub(crate) fn scale_add(x: &[f32], gain: f32, acc: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => scale_add_kernel(simd, x, gain, acc))
}

#[inline(always)]
pub(super) fn scale_add_kernel<S: Simd>(simd: S, x: &[f32], gain: f32, acc: &mut [f32]) -> usize {
    let gain = S::f32s::splat(simd, gain);
    update(simd, acc, [x], |acc, [x]| x.mul_add_precise(gain, acc))
}

pub(crate) fn add(x: &[f32], acc: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => add_kernel(simd, x, acc))
}

#[inline(always)]
pub(super) fn add_kernel<S: Simd>(simd: S, x: &[f32], acc: &mut [f32]) -> usize {
    update(simd, acc, [x], |acc, [x]| acc.add(x))
}
