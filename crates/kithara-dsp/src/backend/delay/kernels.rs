use std::ops::Sub;

use fearless_simd::{f32x16, prelude::*};

#[inline(always)]
pub(crate) fn diffuse_kernel<S: Simd>(
    simd: S,
    samples: &mut [f32],
    history: &[f32],
    gain: f32,
    writes: &mut [f32],
) -> usize {
    let len = samples.len().min(history.len()).min(writes.len());
    let (Some(samples), Some(history), Some(writes)) = (
        samples.get_mut(..len),
        history.get(..len),
        writes.get_mut(..len),
    ) else {
        return 0;
    };
    let gains = f32x16::splat(simd, gain);
    let lanes = f32x16::<S>::LEN;
    let mut inputs = samples.chunks_exact_mut(lanes);
    let mut delayed = history.chunks_exact(lanes);
    let mut outputs = writes.chunks_exact_mut(lanes);
    for ((input, history), output) in (&mut inputs).zip(&mut delayed).zip(&mut outputs) {
        let x = f32x16::from_slice(simd, input);
        let d = f32x16::from_slice(simd, history);
        d.mul_add_precise(gains, x).store_slice(output);
        d.sub(x).store_slice(input);
    }
    let input = inputs.into_remainder();
    if input.is_empty() {
        return len;
    }
    let history = delayed.remainder();
    let output = outputs.into_remainder();
    for ((x, d), w) in input.iter_mut().zip(history).zip(output) {
        *w = d.mul_add(gain, *x);
        *x = *d - *x;
    }
    len
}

#[inline(always)]
pub(crate) fn blend_kernel<S: Simd>(
    simd: S,
    newer: &[f32],
    older: &[f32],
    fraction: f32,
    output: &mut [f32],
) -> usize {
    let len = output.len().min(newer.len()).min(older.len());
    let (Some(newer), Some(older), Some(output)) =
        (newer.get(..len), older.get(..len), output.get_mut(..len))
    else {
        return 0;
    };
    let fractions = f32x16::splat(simd, fraction);
    let lanes = f32x16::<S>::LEN;
    let mut newer = newer.chunks_exact(lanes);
    let mut older = older.chunks_exact(lanes);
    let mut outputs = output.chunks_exact_mut(lanes);
    for ((newer, older), output) in (&mut newer).zip(&mut older).zip(&mut outputs) {
        let a = f32x16::from_slice(simd, newer);
        let b = f32x16::from_slice(simd, older);
        fractions.mul_add_precise(b.sub(a), a).store_slice(output);
    }
    let output = outputs.into_remainder();
    if output.is_empty() {
        return len;
    }
    for ((a, b), output) in newer.remainder().iter().zip(older.remainder()).zip(output) {
        *output = fraction.mul_add(b - a, *a);
    }
    len
}
