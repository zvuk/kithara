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
    let three = f64x2::splat(simd, 3.0);
    let origin = f64x2::splat(simd, left);
    let delta = f64x2::splat(simd, right - left);
    for chunk in positions.chunks_mut(2) {
        let time = f64x2::from_fn(simd, |lane| chunk.get(lane).copied().unwrap_or(0.0));
        let progress = match shape {
            Progression::Hold => zero,
            Progression::Linear => time,
            Progression::Cubic { first, second } => {
                let remaining = one.sub(time);
                let first = f64x2::splat(simd, first);
                let second = f64x2::splat(simd, second);
                let a = three.mul(remaining).mul(remaining).mul(time).mul(first);
                let b = three.mul(remaining).mul(time).mul(time).mul(second);
                a.add(b).add(time.mul(time).mul(time)).max(zero).min(one)
            }
        };
        let interpolated = origin.add(delta.mul(progress));
        for ((position, progress), value) in chunk
            .iter_mut()
            .zip(progress.as_slice())
            .zip(interpolated.as_slice())
        {
            *position = if *position >= 1.0 {
                right
            } else if *position <= 0.0 || left == right || *progress == 0.0 {
                left
            } else if *progress == 1.0 {
                right
            } else {
                *value
            };
        }
    }
}
