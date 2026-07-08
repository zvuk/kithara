use num_traits::cast::ToPrimitive;

use super::{GlideInterpolation, filter};

pub(super) fn interpolate(
    input: &[f32],
    previous: f32,
    position: f64,
    ratio: f64,
    interpolation: GlideInterpolation,
    anti_alias: bool,
) -> f32 {
    let base = position.floor().to_usize().unwrap_or(usize::MAX);
    if base >= input.len() {
        return 0.0;
    }
    let frac = (position - base.to_f64().unwrap_or(0.0))
        .to_f32()
        .unwrap_or(0.0);
    let left = if base == 0 {
        previous
    } else {
        input[base.saturating_sub(1)]
    };
    let center = input[base];
    let right = input.get(base.saturating_add(1)).copied().unwrap_or(0.0);
    let interpolated = match interpolation {
        GlideInterpolation::Linear => linear(center, right, frac),
        GlideInterpolation::Quadratic => quadratic(left, center, right, frac),
    };
    filter::anti_alias_sample(anti_alias, ratio, interpolated, left, center, right)
}

fn linear(center: f32, right: f32, frac: f32) -> f32 {
    center.mul_add(1.0 - frac, right * frac)
}

fn quadratic(left: f32, center: f32, right: f32, frac: f32) -> f32 {
    let slope = 0.5 * (right - left);
    let curve = 0.5 * (right - 2.0 * center + left);
    center + frac * slope + frac * frac * curve
}
