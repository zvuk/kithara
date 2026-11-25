use std::ops::{Add, Mul, Sub};

use fearless_simd::{Level, dispatch, prelude::*};

#[cfg(any(test, feature = "routes"))]
use crate::curve::CurveShape;
use crate::{
    backend::route::{route, table},
    curve::Progression,
};

pub(crate) fn interpolate(shape: Progression, positions: &mut [f64], endpoints: [f64; 2]) {
    match shape {
        Progression::Hold => route!(table::parameter_hold => (positions, endpoints)),
        Progression::Linear => route!(table::parameter_linear => (positions, endpoints)),
        Progression::Cubic { .. } => {
            dispatch!(Level::new(), simd => kernel(simd, shape, positions, endpoints));
        }
    }
}

#[cfg(any(test, feature = "routes"))]
pub(in crate::backend) fn hold(positions: &mut [f64], endpoints: [f64; 2]) {
    CurveShape::HOLD.process(positions, endpoints);
}

#[cfg(any(test, feature = "routes"))]
pub(in crate::backend) fn linear(positions: &mut [f64], endpoints: [f64; 2]) {
    CurveShape::LINEAR.process(positions, endpoints);
}

#[inline(always)]
pub(in crate::backend) fn hold_kernel<S: Simd>(
    simd: S,
    positions: &mut [f64],
    endpoints: [f64; 2],
) {
    kernel(simd, Progression::Hold, positions, endpoints);
}

#[inline(always)]
pub(in crate::backend) fn linear_kernel<S: Simd>(
    simd: S,
    positions: &mut [f64],
    endpoints: [f64; 2],
) {
    kernel(simd, Progression::Linear, positions, endpoints);
}

#[inline(always)]
pub(in crate::backend) fn kernel<S: Simd>(
    simd: S,
    shape: Progression,
    positions: &mut [f64],
    [left, right]: [f64; 2],
) {
    let shape = if left == right {
        if left.to_bits() == right.to_bits() {
            positions.fill(left);
            return;
        }
        Progression::Hold
    } else {
        shape
    };
    let origin = S::f64s::splat(simd, left);
    let end = S::f64s::splat(simd, right);
    let one = S::f64s::splat(simd, 1.0);
    let blend = |progress: S::f64s| {
        if left.is_sign_negative() != right.is_sign_negative()
            && left.abs() > f64::MAX - right.abs()
        {
            origin.mul(one.sub(progress)).add(end.mul(progress))
        } else {
            origin.add(S::f64s::splat(simd, right - left).mul(progress))
        }
    };
    match shape {
        Progression::Hold => write_with(simd, positions, |time| {
            time.simd_ge(one).select(end, origin)
        }),
        Progression::Linear => {
            let zero = S::f64s::splat(simd, 0.0);
            write_with(simd, positions, |time| {
                let value = blend(time);
                let value = time.simd_le(zero).select(origin, value);
                time.simd_ge(one).select(end, value)
            });
        }
        Progression::Cubic { first, second } => {
            let zero = S::f64s::splat(simd, 0.0);
            let three = S::f64s::splat(simd, 3.0);
            let first = S::f64s::splat(simd, first);
            let second = S::f64s::splat(simd, second);
            write_with(simd, positions, |time| {
                let remaining = one.sub(time);
                let a = three.mul(remaining).mul(remaining).mul(time).mul(first);
                let b = three.mul(remaining).mul(time).mul(time).mul(second);
                let progress = a.add(b).add(time.mul(time).mul(time)).max(zero).min(one);
                let value = blend(progress);
                let value = progress.simd_eq(one).select(end, value);
                let value = (time.simd_le(zero) | progress.simd_eq(zero)).select(origin, value);
                time.simd_ge(one).select(end, value)
            });
        }
    }
}

#[inline(always)]
fn write_with<S: Simd>(simd: S, positions: &mut [f64], interpolate: impl Fn(S::f64s) -> S::f64s) {
    let mut chunks = positions.chunks_exact_mut(S::f64s::LEN);
    for chunk in &mut chunks {
        interpolate(S::f64s::from_slice(simd, chunk)).store_slice(chunk);
    }
    let tail = chunks.into_remainder();
    if !tail.is_empty() {
        let time = S::f64s::from_fn(simd, |lane| tail.get(lane).copied().unwrap_or(0.0));
        let values = interpolate(time);
        for (slot, value) in tail.iter_mut().zip(values.as_slice()) {
            *slot = *value;
        }
    }
}
