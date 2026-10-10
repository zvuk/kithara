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
