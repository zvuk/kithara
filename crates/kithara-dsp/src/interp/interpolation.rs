use num_traits::ToPrimitive;

use super::InterpError;
use crate::backend::platform;

/// How [`interpolate`] reads the window around a position `p`, with
/// `b = ⌊p⌋` and `x = p − b`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interpolation {
    /// Chord through `b` and `b + 1`.
    Linear,
    /// Parabola through `b − 1`, `b` and `b + 1`.
    #[default]
    Quadratic,
    /// Catmull-Rom cubic over `b − 1 … b + 2`.
    Hermite,
    /// Watte's tri-linear parabola over `b − 1 … b + 2`.
    Watte,
}

impl Interpolation {
    /// Frames read before and after `b`: a position is valid when
    /// `before ≤ p < len − after`.
    #[must_use]
    pub const fn padding(self) -> (u8, u8) {
        match self {
            Self::Linear => (0, 1),
            Self::Quadratic => (1, 1),
            Self::Hermite | Self::Watte => (1, 2),
        }
    }
}

/// Reads `window` at `positions[k]` into `output[k]` over the common prefix
/// of `positions` and `output`; returns its length.
///
/// # Errors
/// [`InterpError::OutOfWindow`] when a position is `NaN` or outside the range
/// [`Interpolation::padding`] allows, or `window` is longer than `2²⁴`
/// frames; `output` is then untouched.
pub fn interpolate(
    method: Interpolation,
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
) -> Result<usize, InterpError> {
    platform::interpolate(method, window, positions, output)
}

/// Evaluates the parabola through `[previous, current, next]` at `fraction`
/// from the current tap toward the next. Arithmetic stays in `f64` and rounds
/// once to `f32`, without adding the fraction to a window index.
#[must_use]
#[inline]
pub fn quadratic(taps: [f32; 3], fraction: f64) -> f32 {
    let [previous, current, next] = taps.map(f64::from);
    crate::backend::quadratic([previous, current, next, 0.0], fraction)
        .to_f32()
        .unwrap_or(f32::NAN)
}

/// Fills `weights` with Hann-windowed sinc weights with cutoff `cutoff` at the
/// distances `first`, `first + 1`, ... in order.
///
/// `window_radius` is the positive distance from the window's center to its
/// zero. Callers supply finite distances inside that radius and a cutoff in
/// `(0, 1]`.
///
/// Both the sinc numerator and the window advance by a fixed angle per tap, so
/// each is rotated from its first tap instead of evaluated: four sine/cosine
/// pairs per call however many taps.
/// The rotation multiplies and adds plainly: `mul_add` lowers to software FMA
/// on x86 without the FMA target feature.
#[inline(never)]
pub fn sinc_hann_weights(first: f64, cutoff: f64, window_radius: f64, weights: &mut [f64]) {
    use std::f64::consts::PI;

    let (mut sine, mut cosine) = (PI * first * cutoff).sin_cos();
    let (step_sine, step_cosine) = (PI * cutoff).sin_cos();
    let (mut window_sine, mut window_cosine) = (PI * first / window_radius).sin_cos();
    let (window_step_sine, window_step_cosine) = (PI / window_radius).sin_cos();
    let mut distance = first;
    for weight in weights {
        let sinc = if distance * cutoff == 0.0 {
            cutoff
        } else {
            sine / (PI * distance)
        };
        *weight = sinc * (1.0 + window_cosine) * 0.5;
        (sine, cosine) = (
            sine * step_cosine + cosine * step_sine,
            cosine * step_cosine - sine * step_sine,
        );
        (window_sine, window_cosine) = (
            window_sine * window_step_cosine + window_cosine * window_step_sine,
            window_cosine * window_step_cosine - window_sine * window_step_sine,
        );
        distance += 1.0;
    }
}
