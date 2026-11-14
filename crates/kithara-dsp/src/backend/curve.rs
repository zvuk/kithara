use std::ops::{Add, Mul, Sub};

use fearless_simd::{Level, dispatch, f64x2, prelude::*};

use crate::curve::Progression;

pub(crate) fn interpolate(shape: Progression, positions: &mut [f64], endpoints: [f64; 2]) {
    dispatch!(Level::new(), simd => kernel(simd, shape, positions, endpoints));
}

pub(super) fn kernel<S: Simd>(
    simd: S,
    shape: Progression,
    positions: &mut [f64],
    [left, right]: [f64; 2],
) {
    let zero = f64x2::splat(simd, 0.0);
    let one = f64x2::splat(simd, 1.0);
    if left == right {
        if left.to_bits() == right.to_bits() {
            positions.fill(left);
        } else {
            write_with(simd, positions, [left, right], |_| zero);
        }
        return;
    }
    match shape {
        Progression::Hold => write_with(simd, positions, [left, right], |_| zero),
        Progression::Linear => write_with(simd, positions, [left, right], |time| time),
        Progression::Cubic { first, second } => {
            let three = f64x2::splat(simd, 3.0);
            let first = f64x2::splat(simd, first);
            let second = f64x2::splat(simd, second);
            write_with(simd, positions, [left, right], |time| {
                let remaining = one.sub(time);
                let a = three.mul(remaining).mul(remaining).mul(time).mul(first);
                let b = three.mul(remaining).mul(time).mul(time).mul(second);
                a.add(b).add(time.mul(time).mul(time)).max(zero).min(one)
            });
        }
    }
}

fn write_with<S: Simd>(
    simd: S,
    positions: &mut [f64],
    [left, right]: [f64; 2],
    progression: impl Fn(f64x2<S>) -> f64x2<S>,
) {
    let zero = f64x2::splat(simd, 0.0);
    let one = f64x2::splat(simd, 1.0);
    let origin = f64x2::splat(simd, left);
    let end = f64x2::splat(simd, right);
    let delta = f64x2::splat(simd, right - left);
    for chunk in positions.chunks_mut(2) {
        let time = f64x2::from_fn(simd, |lane| chunk.get(lane).copied().unwrap_or(0.0));
        let progress = progression(time);
        let interpolated = origin.add(delta.mul(progress));
        let value = progress.simd_eq(one).select(end, interpolated);
        let value = (time.simd_le(zero) | progress.simd_eq(zero)).select(origin, value);
        let value = time.simd_ge(one).select(end, value);
        if let Some(values) = value.as_slice().get(..chunk.len()) {
            chunk.copy_from_slice(values);
        }
    }
}
