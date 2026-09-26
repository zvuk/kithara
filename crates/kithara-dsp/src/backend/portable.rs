use std::num::NonZeroUsize;

use fearless_simd::{Level, dispatch, prelude::*};

use super::traits::{Backend, sealed};

/// Kernels on `fearless_simd` at the SIMD level chosen once at construction.
#[derive(Clone, Copy, Debug)]
pub struct Portable {
    level: Level,
}

impl Portable {
    /// Runs every kernel at `level`.
    #[must_use]
    pub const fn new(level: Level) -> Self {
        Self { level }
    }
}

impl Default for Portable {
    /// The best level this CPU supports.
    fn default() -> Self {
        Self::new(Level::new())
    }
}

impl sealed::Sealed for Portable {}

impl Backend for Portable {
    fn deinterleave_pair(&self, input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
        dispatch!(self.level, simd => deinterleave_pair_kernel(simd, input, left, right))
    }

    fn gather(&self, input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
        gather_kernel(input, stride, plane)
    }

    fn interleave_pair(&self, left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
        dispatch!(self.level, simd => interleave_pair_kernel(simd, left, right, output))
    }

    fn sanitize(&self, samples: &mut [f32]) {
        dispatch!(self.level, simd => sanitize_kernel(simd, samples));
    }

    fn scatter(&self, plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
        scatter_kernel(plane, output, stride)
    }
}

#[inline(always)]
fn interleave_pair_kernel<S: Simd>(
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
fn deinterleave_pair_kernel<S: Simd>(
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

#[inline(always)]
fn scatter_kernel(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
    let frames = plane.len().min(output.len().div_ceil(stride.get()));
    for (slot, sample) in output.iter_mut().step_by(stride.get()).zip(plane) {
        *slot = *sample;
    }
    frames
}

#[inline(always)]
fn gather_kernel(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
    let frames = plane.len().min(input.len().div_ceil(stride.get()));
    for (slot, sample) in plane.iter_mut().zip(input.iter().step_by(stride.get())) {
        *slot = *sample;
    }
    frames
}

#[inline(always)]
fn sanitize_kernel<S: Simd>(simd: S, samples: &mut [f32]) {
    let smallest = S::f32s::splat(simd, f32::MIN_POSITIVE);
    let largest = S::f32s::splat(simd, f32::MAX);
    let zero = S::f32s::splat(simd, 0.0);
    let mut blocks = samples.chunks_exact_mut(S::f32s::LEN);
    for block in &mut blocks {
        sanitized::<S>(S::f32s::from_slice(simd, block), smallest, largest, zero)
            .store_slice(block);
    }
    let tail = blocks.into_remainder();
    let cleaned = sanitized::<S>(padded(simd, tail), smallest, largest, zero);
    for (slot, sample) in tail.iter_mut().zip(cleaned.as_slice()) {
        *slot = *sample;
    }
}

/// `x` where `MIN_POSITIVE <= |x| <= MAX`, `+0.0` elsewhere; `NaN` fails both tests.
#[inline(always)]
fn sanitized<S: Simd>(x: S::f32s, smallest: S::f32s, largest: S::f32s, zero: S::f32s) -> S::f32s {
    let magnitude = x.abs();
    let normal = magnitude.simd_ge(smallest).select(x, zero);
    magnitude.simd_le(largest).select(normal, zero)
}

/// A vector holding `tail` in its first lanes and `+0.0` in the rest.
#[inline(always)]
fn padded<S: Simd>(simd: S, tail: &[f32]) -> S::f32s {
    let mut vector = S::f32s::splat(simd, 0.0);
    for (slot, sample) in vector.as_mut_slice().iter_mut().zip(tail) {
        *slot = *sample;
    }
    vector
}
