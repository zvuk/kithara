use std::{
    f32::consts::FRAC_PI_2,
    f64::consts::{FRAC_PI_2 as QUARTER_TURN, FRAC_PI_4 as EIGHTH_TURN},
    ops::{Mul, Sub},
};

use fearless_simd::{Level, dispatch, prelude::*};

use super::simd::{blocks, store, vector};
use crate::fade::FadeCurve;

mod consts {
    /// Progress at or below which the fade is fully outgoing (firewheel's edge).
    pub(super) const START: f32 = 0.000_01;
    /// Progress at or above which the fade is fully incoming (firewheel's edge).
    pub(super) const END: f32 = 0.999_99;
    /// Taylor coefficients of `(sin a − a)/a³` in powers of `a²`, to `a¹¹`:
    /// within `10⁻¹¹` of `sin a` for `|a| ≤ π/4`.
    pub(super) const SINE: [f64; 5] = [
        -1.0 / 6.0,
        1.0 / 120.0,
        -1.0 / 5_040.0,
        1.0 / 362_880.0,
        -1.0 / 39_916_800.0,
    ];
    /// Taylor coefficients of `(cos a − 1)/a²` in powers of `a²`, to `a¹⁰`:
    /// within `2·10⁻¹⁰` of `cos a` for `|a| ≤ π/4`.
    pub(super) const COSINE: [f64; 5] = [
        -1.0 / 2.0,
        1.0 / 24.0,
        -1.0 / 720.0,
        1.0 / 40_320.0,
        -1.0 / 3_628_800.0,
    ];
}

pub(crate) fn gains(
    curve: FadeCurve,
    positions: &[f32],
    outgoing: &mut [f32],
    incoming: &mut [f32],
) -> usize {
    dispatch!(Level::new(), simd => gains_kernel(simd, curve, positions, outgoing, incoming))
}

#[inline(always)]
pub(super) fn gains_kernel<S: Simd>(
    simd: S,
    curve: FadeCurve,
    positions: &[f32],
    outgoing: &mut [f32],
    incoming: &mut [f32],
) -> usize {
    let len = positions.len().min(outgoing.len()).min(incoming.len());
    blocks::<S>(len, |start, end| {
        if let (Some(position), Some(out), Some(into)) = (
            positions.get(start..end),
            outgoing.get_mut(start..end),
            incoming.get_mut(start..end),
        ) {
            let (from, to) = pair(simd, curve, vector(simd, position));
            store::<S>(from, out);
            store::<S>(to, into);
        }
    });
    len
}

/// The outgoing and incoming gains at progress `u`, as
/// `FadeCurve::compute_gains_0_to_1` defines them.
#[inline(always)]
fn pair<S: Simd>(simd: S, curve: FadeCurve, u: S::f32s) -> (S::f32s, S::f32s) {
    let one = S::f32s::splat(simd, 1.0);
    let zero = S::f32s::splat(simd, 0.0);
    let (from, to) = match curve {
        FadeCurve::EqualPower3dB => quarter_wave(simd, u, |gain| gain),
        FadeCurve::EqualPower6dB => quarter_wave(simd, u, |gain| gain.mul(gain)),
        FadeCurve::SquareRoot => (one.sub(u).sqrt(), u.sqrt()),
        FadeCurve::Linear => (one.sub(u), u),
    };
    let start = u.simd_le(S::f32s::splat(simd, consts::START));
    let end = u.simd_ge(S::f32s::splat(simd, consts::END));
    (
        start.select(one, end.select(zero, from)),
        start.select(zero, end.select(one, to)),
    )
}

/// `(shape(cos x), shape(sin x))` at `x = π/2·u`: `x` rounds to `f32` as
/// firewheel's does, the rest runs in `f64` and rounds once at the end.
#[inline(always)]
fn quarter_wave<S: Simd>(
    simd: S,
    u: S::f32s,
    shape: impl Fn(S::f64s) -> S::f64s,
) -> (S::f32s, S::f32s) {
    let (low, high) = u.mul(S::f32s::splat(simd, FRAC_PI_2)).widen();
    let (low_cos, low_sin) = cos_sin(simd, low);
    let (high_cos, high_sin) = cos_sin(simd, high);
    (
        shape(low_cos).narrow(shape(high_cos)),
        shape(low_sin).narrow(shape(high_sin)),
    )
}

/// `(cos x, sin x)` for `x` in `[0, π/2]`: above `π/4` the polynomials run on
/// `π/2 − x`, an exact `f64` subtraction, so the side tending to zero keeps
/// its relative precision.
#[inline(always)]
fn cos_sin<S: Simd>(simd: S, x: S::f64s) -> (S::f64s, S::f64s) {
    let near = x.simd_le(S::f64s::splat(simd, EIGHTH_TURN));
    let a = near.select(x, S::f64s::splat(simd, QUARTER_TURN).sub(x));
    let z = a.mul(a);
    let sine = a.mul(z).mul_add(horner(simd, z, consts::SINE), a);
    let cosine = z.mul_add(horner(simd, z, consts::COSINE), S::f64s::splat(simd, 1.0));
    (near.select(cosine, sine), near.select(sine, cosine))
}

/// `Σ c[k]·zᵏ`, written out: the size profile neither unrolls nor inlines a
/// fold over the coefficients.
#[inline(always)]
fn horner<S: Simd>(simd: S, z: S::f64s, [c0, c1, c2, c3, c4]: [f64; 5]) -> S::f64s {
    S::f64s::splat(simd, c4)
        .mul_add(z, S::f64s::splat(simd, c3))
        .mul_add(z, S::f64s::splat(simd, c2))
        .mul_add(z, S::f64s::splat(simd, c1))
        .mul_add(z, S::f64s::splat(simd, c0))
}
