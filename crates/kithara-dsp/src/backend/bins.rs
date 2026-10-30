use std::{
    f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU},
    ops::{Add, Div, Mul, Sub},
};

use fearless_simd::{Level, dispatch, prelude::*};

use super::simd::zip_map;

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

/// Scalar transcendentals and ordered accumulation preserve the consumer's bits.
pub(crate) fn prediction_difference(
    [re, im]: [&[f32]; 2],
    [observed, angle]: [&[f32]; 2],
    [magnitude, phase, phase_step]: [&mut [f32]; 3],
) -> f32 {
    let mut total = 0.0;
    for ((((re, im), (observed, angle)), (magnitude, phase)), step) in re
        .iter()
        .zip(im)
        .zip(observed.iter().zip(angle))
        .zip(magnitude.iter_mut().zip(phase.iter_mut()))
        .zip(phase_step.iter_mut())
    {
        let (sin, cos) = wrap(*phase + *step).sin_cos();
        total += (re - *magnitude * cos).hypot(im - *magnitude * sin);
        *step = wrap(angle - *phase);
        *phase = *angle;
        *magnitude = *observed;
    }
    total
}

fn wrap(angle: f32) -> f32 {
    angle - TAU * (angle / TAU).round()
}
