use fearless_simd::prelude::*;

/// A vector holding `tail` in its first lanes and `+0.0` in the rest.
#[inline(always)]
pub(super) fn padded<S: Simd>(simd: S, tail: &[f32]) -> S::f32s {
    let mut vector = S::f32s::splat(simd, 0.0);
    for (slot, sample) in vector.as_mut_slice().iter_mut().zip(tail) {
        *slot = *sample;
    }
    vector
}
