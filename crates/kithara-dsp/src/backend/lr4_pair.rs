#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use std::arch::aarch64::{
    float32x2_t, vadd_f32, vdup_n_f32, vget_lane_f32, vmul_f32, vset_lane_f32, vsub_f32,
};

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
use fearless_simd::Level;

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
#[inline]
#[target_feature(enable = "neon")]
fn pair([low, high]: [f32; 2]) -> float32x2_t {
    vset_lane_f32::<1>(high, vdup_n_f32(low))
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
fearless_simd::kernel!(
    #[inline(always)]
    fn evaluate_neon(
        neon: Neon,
        c: &[[f32; 2]; 5],
        state: &[[f32; 2]; 4],
        input: [f32; 2],
    ) -> [f32; 2] {
        let [b0, b1, b2, a1, a2] = *c;
        let [x1, x2, y1, y2] = *state;
        let value = vmul_f32(pair(b0), pair(input));
        let value = vadd_f32(value, vmul_f32(pair(b1), pair(x1)));
        let value = vadd_f32(value, vmul_f32(pair(b2), pair(x2)));
        let value = vsub_f32(value, vmul_f32(pair(a1), pair(y1)));
        let value = vsub_f32(value, vmul_f32(pair(a2), pair(y2)));
        [vget_lane_f32::<0>(value), vget_lane_f32::<1>(value)]
    }
);

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
#[inline(always)]
pub(crate) fn evaluate(c: &[[f32; 2]; 5], state: &[[f32; 2]; 4], input: [f32; 2]) -> [f32; 2] {
    let neon = Level::baseline()
        .as_neon()
        .expect("NEON is the configured target baseline");
    evaluate_neon(neon, c, state, input)
}

#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
#[inline(always)]
pub(crate) fn evaluate(c: &[[f32; 2]; 5], state: &[[f32; 2]; 4], [l, h]: [f32; 2]) -> [f32; 2] {
    let [[b00, b01], [b10, b11], [b20, b21], [a10, a11], [a20, a21]] = *c;
    let [[x10, x11], [x20, x21], [y10, y11], [y20, y21]] = *state;
    [
        b00 * l + b10 * x10 + b20 * x20 - a10 * y10 - a20 * y20,
        b01 * h + b11 * x11 + b21 * x21 - a11 * y11 - a21 * y21,
    ]
}
