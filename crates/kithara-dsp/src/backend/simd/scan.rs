use fearless_simd::prelude::*;

#[inline(always)]
pub(crate) fn kernel<S: Simd>(simd: S, samples: &[f32], limit: f32) -> Option<usize> {
    let bound = S::f32s::splat(simd, limit);
    let chunks = samples.chunks_exact(S::f32s::LEN);
    let tail = chunks.remainder();
    if !tail.is_empty() {
        let lanes = u32::try_from(tail.len()).expect("SIMD width fits u32");
        let valid = u64::MAX.wrapping_shr(u64::BITS.saturating_sub(lanes));
        let loud = padded(simd, tail).abs().simd_gt(bound).to_bitmask() & valid;
        if loud != 0 {
            let lane = usize::try_from(loud.ilog2()).expect("SIMD lane fits usize");
            return Some(samples.len().wrapping_sub(tail.len()).wrapping_add(lane));
        }
    }
    for (block, values) in chunks.enumerate().rev() {
        let loud = S::f32s::from_slice(simd, values)
            .abs()
            .simd_gt(bound)
            .to_bitmask();
        if loud != 0 {
            let lane = usize::try_from(loud.ilog2()).expect("SIMD lane fits usize");
            return Some(block.wrapping_mul(S::f32s::LEN).wrapping_add(lane));
        }
    }
    None
}

/// A vector holding `tail` in its first lanes and `+0.0` in the rest.
#[inline(always)]
pub(crate) fn padded<S: Simd>(simd: S, tail: &[f32]) -> S::f32s {
    let mut vector = S::f32s::splat(simd, 0.0);
    for (slot, sample) in vector.as_mut_slice().iter_mut().zip(tail) {
        *slot = *sample;
    }
    vector
}
