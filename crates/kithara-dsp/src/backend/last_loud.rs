use fearless_simd::prelude::*;

use super::load::padded;

#[inline(always)]
pub(super) fn kernel<S: Simd>(simd: S, samples: &[f32], limit: f32) -> Option<usize> {
    let bound = S::f32s::splat(simd, limit);
    for (block, values) in samples.chunks(S::f32s::LEN).enumerate().rev() {
        let lanes = u32::try_from(values.len()).expect("SIMD width fits u32");
        let valid = u64::MAX.wrapping_shr(u64::BITS.saturating_sub(lanes));
        let loud = padded(simd, values).abs().simd_gt(bound).to_bitmask() & valid;
        if loud != 0 {
            let lane = usize::try_from(loud.ilog2()).expect("SIMD lane fits usize");
            return Some(block.saturating_mul(S::f32s::LEN).saturating_add(lane));
        }
    }
    None
}
