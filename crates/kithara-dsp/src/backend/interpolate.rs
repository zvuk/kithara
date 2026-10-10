use num_traits::ToPrimitive;

use crate::{
    consts,
    interp::{InterpError, Interpolation},
};

/// `interp::interpolate` in scalar code: the taps of each position are a
/// gather, and moving them lane by lane into vectors costs more than the
/// formula saves.
pub(crate) fn interpolate(
    method: Interpolation,
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
) -> Result<usize, InterpError> {
    let frames = check(method, window, positions, output)?;
    match method {
        Interpolation::Linear => kernel(window, positions, output, linear),
        Interpolation::Quadratic => kernel(window, positions, output, quadratic),
        Interpolation::Hermite => kernel(window, positions, output, hermite),
        Interpolation::Watte => kernel(window, positions, output, watte),
    }
    Ok(frames)
}

/// Length of the common prefix of `positions` and `output` when every
/// position in it lies in `before ≤ p < len − after`. Scans without an early
/// exit, so the loop vectorizes; `NaN` fails both comparisons.
fn check(
    method: Interpolation,
    window: &[f32],
    positions: &[f32],
    output: &[f32],
) -> Result<usize, InterpError> {
    let frames = positions.len().min(output.len());
    if frames == 0 {
        return Ok(0);
    }
    let (before, after) = method.padding();
    let len = u32::try_from(window.len())
        .ok()
        .filter(|len| *len <= consts::MAX_WINDOW)
        .ok_or(InterpError::OutOfWindow)?;
    let low = f64::from(before);
    let high = f64::from(len) - f64::from(after);
    let inside = positions
        .iter()
        .take(frames)
        .fold(true, |inside, position| {
            let position = f64::from(*position);
            inside & (position >= low) & (position < high)
        });
    inside.then_some(frames).ok_or(InterpError::OutOfWindow)
}

/// Evaluates `eval` on the taps `[y(b − 1), y(b), y(b + 1), y(b + 2)]` at
/// `x = p − b`, `b = ⌊p⌋`. A tap outside the window reads zero; the caller
/// has checked that every tap a method uses lies inside. The formulas run in
/// `f64` and round once to `f32`: neighbours of opposite sign cancel in the
/// coefficients, and `f32` steps there lose more than four epsilons. They use
/// `*` and `+`: a scalar `mul_add` on a target without FMA is a libm call.
#[inline(always)]
fn kernel(
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
    eval: impl Fn([f64; 4], f64) -> f64,
) {
    for (position, slot) in positions.iter().zip(output.iter_mut()) {
        let base = position.floor();
        let first = base.to_usize().unwrap_or(usize::MAX).wrapping_sub(1);
        let tap = |offset: usize| {
            window
                .get(first.wrapping_add(offset))
                .map_or(0.0, |sample| f64::from(*sample))
        };
        let x = f64::from(*position) - f64::from(base);
        *slot = eval([tap(0), tap(1), tap(2), tap(3)], x)
            .to_f32()
            .unwrap_or(f32::NAN);
    }
}

/// `y0 + x·(y1 − y0)`.
#[inline(always)]
fn linear([_, y0, y1, _]: [f64; 4], x: f64) -> f64 {
    x * (y1 - y0) + y0
}

/// The parabola through three taps: `(c2·x + c1)·x + y0` with
/// `c1 = (y1 − ym1)/2` and `c2 = (y1 − 2·y0 + ym1)/2`.
#[inline(always)]
pub(crate) fn quadratic([ym1, y0, y1, _]: [f64; 4], x: f64) -> f64 {
    let c1 = 0.5 * (y1 - ym1);
    let c2 = 0.5 * (y1 - 2.0 * y0 + ym1);
    (c2 * x + c1) * x + y0
}

/// Catmull-Rom: `((c3·x + c2)·x + c1)·x + y0`.
#[inline(always)]
fn hermite([ym1, y0, y1, y2]: [f64; 4], x: f64) -> f64 {
    let c1 = 0.5 * (y1 - ym1);
    let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
    let c3 = 1.5 * (y0 - y1) + 0.5 * (y2 - ym1);
    ((c3 * x + c2) * x + c1) * x + y0
}

/// `(c2·x + c1)·x + y0` with `c1 = 1.5·y1 − (y0 + ym1 + y2)/2` and
/// `c2 = (ym1 + y2 − y0 − y1)/2`.
#[inline(always)]
fn watte([ym1, y0, y1, y2]: [f64; 4], x: f64) -> f64 {
    let outer = ym1 + y2;
    let c1 = 1.5 * y1 - 0.5 * (y0 + outer);
    let c2 = 0.5 * (outer - y0 - y1);
    (c2 * x + c1) * x + y0
}
