use super::ffi::{vDSP_vlint, vDSP_vqint};

mod consts {
    /// Positions are `f32`: every frame index below `2²⁴` is exact.
    pub(super) const MAX_WINDOW: u32 = 1 << 24;
}

/// A position lies outside the frames the interpolation may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("interpolation position lies outside the window")]
pub struct OutOfWindow;

type Render = unsafe extern "C" fn(*const f32, *const f32, isize, *mut f32, isize, usize, usize);

/// A vDSP interpolation and the lowest position it may read at.
#[derive(Clone, Copy)]
struct Method {
    before: f64,
    render: Render,
}

/// `vDSP_vlint`: `output[k]` lies on the chord through `window[b]` and
/// `window[b + 1]`, `b = ⌊positions[k]⌋`. Writes the common prefix of
/// `positions` and `output` and returns its length.
///
/// # Errors
/// [`OutOfWindow`] when a position is `NaN` or outside `0 ≤ p < len − 1`, or
/// the window is longer than `2²⁴` frames; `output` is then untouched.
pub fn linear_interpolate_f32(
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
) -> Result<usize, OutOfWindow> {
    interpolate(
        Method {
            before: 0.0,
            render: vDSP_vlint,
        },
        window,
        positions,
        output,
    )
}

/// `vDSP_vqint`: `output[k]` lies on the parabola through `window[b − 1]`,
/// `window[b]` and `window[b + 1]`, `b = ⌊positions[k]⌋`. Writes the common
/// prefix of `positions` and `output` and returns its length.
///
/// # Errors
/// [`OutOfWindow`] when a position is `NaN` or outside `1 ≤ p < len − 1`, or
/// the window is longer than `2²⁴` frames; `output` is then untouched.
pub fn quadratic_interpolate_f32(
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
) -> Result<usize, OutOfWindow> {
    interpolate(
        Method {
            before: 1.0,
            render: vDSP_vqint,
        },
        window,
        positions,
        output,
    )
}

/// Checks every position in one pass without an early exit, so the scan
/// vectorizes; `NaN` fails both comparisons.
fn interpolate(
    method: Method,
    window: &[f32],
    positions: &[f32],
    output: &mut [f32],
) -> Result<usize, OutOfWindow> {
    let frames = positions.len().min(output.len());
    if frames == 0 {
        return Ok(0);
    }
    let len = u32::try_from(window.len())
        .ok()
        .filter(|len| *len <= consts::MAX_WINDOW)
        .ok_or(OutOfWindow)?;
    let high = f64::from(len) - 1.0;
    let inside = positions
        .iter()
        .take(frames)
        .fold(true, |inside, position| {
            let position = f64::from(*position);
            inside & (position >= method.before) & (position < high)
        });
    if !inside {
        return Err(OutOfWindow);
    }
    // SAFETY: every position lies in [before, len - 1), so vDSP reads window[0..len].
    // SAFETY: frames bounds positions and output.
    // SAFETY: window, positions and output are contiguous f32 slices.
    unsafe {
        (method.render)(
            window.as_ptr(),
            positions.as_ptr(),
            1,
            output.as_mut_ptr(),
            1,
            frames,
            window.len(),
        );
    }
    Ok(frames)
}
