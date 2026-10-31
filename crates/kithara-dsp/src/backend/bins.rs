use std::{
    f32::consts::{FRAC_PI_2, FRAC_PI_4, PI},
    ops::{Add, Div, Mul, Sub},
};

use fearless_simd::{Level, dispatch, prelude::*};

use super::simd::{blocks, store, vector, zip_map};

mod consts {
    /// Cephes `atanf` on `[0, tan(π/8)]`: `atan(t) ≈ t + t³·P(t²)`, the
    /// coefficients of `P` from the highest power down.
    pub(super) const ATAN_POLY: [f32; 4] =
        [0.080_537_446, -0.138_776_85, 0.199_777_11, -0.333_329_5];
    /// Above `tan(π/8)` a ratio folds through
    /// `atan(r) = π/4 + atan((r − 1) / (r + 1))`.
    pub(super) const TAN_PI_8: f32 = 0.414_213_57;
}

/// `√(re² + im²)` per bin: within two epsilons of `hypot` while the squares
/// stay finite, that is below `1.8e19`.
pub(crate) fn magnitude(re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => magnitude_kernel(simd, re, im, output))
}

#[inline(always)]
pub(super) fn magnitude_kernel<S: Simd>(
    simd: S,
    re: &[f32],
    im: &[f32],
    output: &mut [f32],
) -> usize {
    zip_map(simd, [re, im], output, |x, y| x.mul_add(x, y.mul(y)).sqrt())
}

/// `arg(re + i·im)` per bin in `[−π, π]`, within `1e-6` rad of `atan2`.
pub(crate) fn phase(re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => phase_kernel(simd, re, im, output))
}

#[inline(always)]
pub(super) fn phase_kernel<S: Simd>(simd: S, re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    zip_map(simd, [re, im], output, |x, y| atan2(simd, y, x))
}

/// `atan2(y, x)` with the signs of zero `f32::atan2` gives: the ratio of
/// the smaller magnitude to the larger is folded into `[0, tan(π/8)]`,
/// evaluated by the polynomial and unfolded by octant and quadrant.
#[inline(always)]
fn atan2<S: Simd>(simd: S, y: S::f32s, x: S::f32s) -> S::f32s {
    let zero = S::f32s::splat(simd, 0.0);
    let one = S::f32s::splat(simd, 1.0);
    let (ay, ax) = (y.abs(), x.abs());
    let (low, high) = (ay.min(ax), ay.max(ax));
    let ratio = high.simd_gt(zero).select(low.div(high), zero);
    let reduced = ratio.simd_gt(S::f32s::splat(simd, consts::TAN_PI_8));
    let t = reduced.select(ratio.sub(one).div(ratio.add(one)), ratio);
    let base = reduced.select(S::f32s::splat(simd, FRAC_PI_4), zero);
    let t2 = t.mul(t);
    let poly = consts::ATAN_POLY
        .iter()
        .fold(zero, |acc, &c| acc.mul_add(t2, S::f32s::splat(simd, c)));
    let angle = base.add(t.mul_add(t2.mul(poly), t));
    let angle = ay
        .simd_gt(ax)
        .select(S::f32s::splat(simd, FRAC_PI_2).sub(angle), angle);
    let angle = one
        .copysign(x)
        .simd_lt(zero)
        .select(S::f32s::splat(simd, PI).sub(angle), angle);
    angle.copysign(y)
}

/// `|X − X̂|` summed over the common prefix with
/// `X̂ = X₁²·conj(X₂) / (|X₁|·|X₂|)`, then the bins shift into the history.
pub(crate) fn prediction_difference(bins: [&[f32]; 2], history: [&mut [f32]; 4]) -> f32 {
    dispatch!(Level::new(), simd => prediction_kernel(simd, bins, history))
}

#[inline(always)]
pub(super) fn prediction_kernel<S: Simd>(
    simd: S,
    [re, im]: [&[f32]; 2],
    [previous_re, previous_im, earlier_re, earlier_im]: [&mut [f32]; 4],
) -> f32 {
    let len = re
        .len()
        .min(im.len())
        .min(previous_re.len())
        .min(previous_im.len())
        .min(earlier_re.len())
        .min(earlier_im.len());
    let zero = S::f32s::splat(simd, 0.0);
    let one = S::f32s::splat(simd, 1.0);
    let mut total = zero;
    blocks::<S>(len, |start, end| {
        let load = |plane: &[f32]| vector(simd, plane.get(start..end).unwrap_or_default());
        let (x, y) = (load(re), load(im));
        let (x1, y1) = (load(previous_re), load(previous_im));
        let (x2, y2) = (load(earlier_re), load(earlier_im));
        // A silent earlier bin turns by phase zero; a silent previous one predicts zero.
        let earlier = x2.mul_add(x2, y2.mul(y2)).sqrt();
        let known = earlier.simd_gt(zero);
        let (u, v) = (known.select(x2, one), known.select(y2, zero));
        let previous = x1.mul_add(x1, y1.mul(y1)).sqrt();
        let scale = previous.mul(known.select(earlier, one));
        let (sr, si) = (x1.mul(x1).sub(y1.mul(y1)), x1.add(x1).mul(y1));
        let audible = previous.simd_gt(zero);
        let pr = audible.select(sr.mul_add(u, si.mul(v)).div(scale), zero);
        let pi = audible.select(si.mul(u).sub(sr.mul(v)).div(scale), zero);
        let (dx, dy) = (x.sub(pr), y.sub(pi));
        total = total.add(dx.mul_add(dx, dy.mul(dy)).sqrt());
        for (plane, value) in [
            (&mut *earlier_re, x1),
            (&mut *earlier_im, y1),
            (&mut *previous_re, x),
            (&mut *previous_im, y),
        ] {
            if let Some(block) = plane.get_mut(start..end) {
                store::<S>(value, block);
            }
        }
    });
    total.as_slice().iter().sum()
}
