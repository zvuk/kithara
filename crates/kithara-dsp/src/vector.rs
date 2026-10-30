use std::num::NonZeroUsize;

use crate::backend::{self, platform};

/// `Σx²` over `samples`; `0.0` for an empty slice.
///
/// Within `N·ε·Σx²` of the exact sum. A `NaN` or an infinite sample carries
/// through: the kernel never sanitizes.
#[must_use]
pub fn sum_squares(samples: &[f32]) -> f32 {
    platform::sum_squares(samples)
}

/// `output[i] = start + i*step`, rounded once while `i` stays below 2^24;
/// returns `output.len()`.
pub fn ramp(output: &mut [f32], start: f32, step: f32) -> usize {
    backend::ramp(output, start, step)
}

/// `samples[i] /= divisor`, correctly rounded as IEEE division; returns
/// `samples.len()`.
pub fn div(samples: &mut [f32], divisor: f32) -> usize {
    backend::div(samples, divisor)
}

/// `samples[i] = samples[i]*scale + offset`, within 1 ulp of the exact
/// value; returns `samples.len()`.
pub fn affine(samples: &mut [f32], scale: f32, offset: f32) -> usize {
    platform::affine(samples, scale, offset)
}

/// `x[i] -= y[i]` over the common prefix, correctly rounded; returns its
/// length.
pub fn sub(x: &mut [f32], y: &[f32]) -> usize {
    platform::sub(x, y)
}

/// `acc[i] += x[i]*gain` over the common prefix, within 1 ulp of the exact
/// value; returns its length.
pub fn scale_add(x: &[f32], gain: f32, acc: &mut [f32]) -> usize {
    platform::scale_add(x, gain, acc)
}

/// `acc[i] += x[i]*y[i]`, rounded once, over the common prefix of the three
/// slices; returns its length.
pub fn mul_add(x: &[f32], y: &[f32], acc: &mut [f32]) -> usize {
    backend::mul_add(x, y, acc)
}

/// `acc[i] += x[i]` over the common prefix, correctly rounded; returns its
/// length.
pub fn add(x: &[f32], acc: &mut [f32]) -> usize {
    platform::add(x, acc)
}

/// Blends the common whole-frame prefix of two interleaved signals; returns its frame count.
///
/// Frame `k` uses `g = (first + k) / frames`, forming the integer numerator
/// before conversion to `f32`. Unequal samples use the fused operation
/// `outgoing.mul_add(1.0 - g, incoming * g)`; identical bits are copied exactly,
/// including `NaN` payloads and signed zero. Partial frames and the remaining
/// tail stay untouched. Gains are not clamped; a zero denominator follows IEEE division.
/// No allocation or sanitization occurs.
pub fn crossfade_linear(
    outgoing: &[f32],
    samples: &mut [f32],
    channels: NonZeroUsize,
    first: usize,
    frames: usize,
) -> usize {
    backend::crossfade_linear(outgoing, samples, channels, first, frames)
}
