#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
use std::ops::Sub;

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
use fearless_simd::prelude::*;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
use fearless_simd::{Level, dispatch};

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
use super::simd::{blocks, store, update, vector};

/// Produces `y = d - x` in samples/history and `w = x + g*d` in writes.
pub(crate) fn diffuse(
    samples: &mut [f32],
    history: &mut [f32],
    gain: f32,
    writes: &mut [f32],
) -> usize {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        super::accelerate::delay_diffuse(samples, history, gain, writes)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    dispatch!(Level::new(), simd => diffuse_kernel(simd, samples, history, gain, writes))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
#[inline(always)]
pub(super) fn diffuse_kernel<S: Simd>(
    simd: S,
    samples: &mut [f32],
    history: &mut [f32],
    gain: f32,
    writes: &mut [f32],
) -> usize {
    let len = samples.len().min(history.len()).min(writes.len());
    let gain = S::f32s::splat(simd, gain);
    blocks::<S>(len, |start, end| {
        if let (Some(samples), Some(history), Some(writes)) = (
            samples.get(start..end),
            history.get_mut(start..end),
            writes.get_mut(start..end),
        ) {
            let x = vector(simd, samples);
            let d = vector(simd, history);
            store::<S>(d.sub(x), history);
            store::<S>(d.mul_add_precise(gain, x), writes);
        }
    });
    samples
        .split_at_mut(len)
        .0
        .copy_from_slice(history.split_at(len).0);
    len
}

pub(crate) fn blend(newer: &[f32], older: &[f32], fraction: f32, output: &mut [f32]) -> usize {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        super::accelerate::delay_blend(newer, older, fraction, output)
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    dispatch!(Level::new(), simd => blend_kernel(simd, newer, older, fraction, output))
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "ios"))))]
#[inline(always)]
pub(super) fn blend_kernel<S: Simd>(
    simd: S,
    newer: &[f32],
    older: &[f32],
    fraction: f32,
    output: &mut [f32],
) -> usize {
    let fraction = S::f32s::splat(simd, fraction);
    update(simd, output, [newer, older], |_, [newer, older]| {
        fraction.mul_add_precise(older.sub(newer), newer)
    })
}

/// Pairs contiguous spans on either side of the two read cursors' wraps.
pub(crate) fn read(
    plane: &[f32],
    mut newer: usize,
    mut older: usize,
    fraction: f32,
    output: &mut [f32],
) -> usize {
    if newer >= plane.len() || older >= plane.len() {
        return 0;
    }
    let mut written = 0;
    while written < output.len() {
        let len = output
            .len()
            .wrapping_sub(written)
            .min(plane.len().wrapping_sub(newer))
            .min(plane.len().wrapping_sub(older));
        let next = written.wrapping_add(len);
        blend(
            plane
                .get(newer..newer.wrapping_add(len))
                .unwrap_or_default(),
            plane
                .get(older..older.wrapping_add(len))
                .unwrap_or_default(),
            fraction,
            output.get_mut(written..next).unwrap_or_default(),
        );
        written = next;
        newer = newer.wrapping_add(len);
        older = older.wrapping_add(len);
        if newer == plane.len() {
            newer = 0;
        }
        if older == plane.len() {
            older = 0;
        }
    }
    written
}
