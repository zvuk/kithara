use fearless_simd::{Level, dispatch};

use super::{super::platform, blend_kernel, diffuse_kernel};

pub(crate) fn diffuse(
    samples: &mut [f32],
    history: &[f32],
    gain: f32,
    writes: &mut [f32],
) -> usize {
    dispatch!(Level::new(), simd => diffuse_kernel(simd, samples, history, gain, writes))
}

pub(crate) fn blend(newer: &[f32], older: &[f32], fraction: f32, output: &mut [f32]) -> usize {
    dispatch!(Level::new(), simd => blend_kernel(simd, newer, older, fraction, output))
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
        platform::delay_blend(
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
