use std::ops::{Add, Div, Mul, Sub};

use fearless_simd::{Level, dispatch, prelude::*};

use super::simd::{blocks, store, vector};

/// `|X − X̂|` summed over the common prefix with
/// `X̂ = X₁²·conj(X₂) / (|X₁|·|X₂|)`, then the bins shift into the history.
pub(crate) fn prediction_difference(bins: [&[f32]; 2], history: [&mut [f32]; 4]) -> f32 {
    dispatch!(Level::new(), simd => prediction_kernel(simd, bins, history))
}

#[inline(always)]
fn prediction_kernel<S: Simd>(
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
        let x = vector(simd, re.get(start..end).unwrap_or_default());
        let y = vector(simd, im.get(start..end).unwrap_or_default());
        let x1 = vector(simd, previous_re.get(start..end).unwrap_or_default());
        let y1 = vector(simd, previous_im.get(start..end).unwrap_or_default());
        let x2 = vector(simd, earlier_re.get(start..end).unwrap_or_default());
        let y2 = vector(simd, earlier_im.get(start..end).unwrap_or_default());
        // A silent earlier bin turns by phase zero; a silent previous one predicts zero.
        let earlier = x2.mul_add(x2, y2.mul(y2)).sqrt();
        let known = earlier.simd_gt(zero);
        let (u, v) = (known.select(x2, one), known.select(y2, zero));
        let previous = x1.mul_add(x1, y1.mul(y1)).sqrt();
        let scale = previous.mul(known.select(earlier, one));
        // The unit turn `c + i·s = X₁·conj(X₂) / (|X₁|·|X₂|)` keeps every product quadratic.
        let c = x1.mul_add(u, y1.mul(v)).div(scale);
        let s = y1.mul(u).sub(x1.mul(v)).div(scale);
        let audible = previous.simd_gt(zero);
        let pr = audible.select(x1.mul(c).sub(y1.mul(s)), zero);
        let pi = audible.select(x1.mul_add(s, y1.mul(c)), zero);
        let (dx, dy) = (x.sub(pr), y.sub(pi));
        total = total.add(dx.mul_add(dx, dy.mul(dy)).sqrt());
        if let Some(block) = earlier_re.get_mut(start..end) {
            store::<S>(x1, block);
        }
        if let Some(block) = earlier_im.get_mut(start..end) {
            store::<S>(y1, block);
        }
        if let Some(block) = previous_re.get_mut(start..end) {
            store::<S>(x, block);
        }
        if let Some(block) = previous_im.get_mut(start..end) {
            store::<S>(y, block);
        }
    });
    total.as_slice().iter().sum()
}
