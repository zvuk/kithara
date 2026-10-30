use num_traits::AsPrimitive;

use super::InterpError;

/// Interpolates the common channel prefix of two frames into `output` and
/// returns its length, leaving the output tail untouched.
///
/// A zero `fraction` copies the current frame bit for bit. Otherwise each
/// channel evaluates `(next - current).mul_add(fraction, current)` in `f64`
/// before rounding once to `f32`. The fraction is independent of any integer
/// source coordinate; samples and fractions are not sanitized.
#[inline]
pub fn interpolate_frame(
    current: &[f32],
    next: &[f32],
    fraction: f64,
    output: &mut [f32],
) -> usize {
    let channels = current.len().min(next.len()).min(output.len());
    if fraction == 0.0 {
        for (sample, &current) in output.iter_mut().zip(current).take(channels) {
            *sample = current;
        }
    } else {
        for ((&current, &next), sample) in current.iter().zip(next).zip(output.iter_mut()) {
            let current = f64::from(current);
            let next = f64::from(next);
            *sample = (next - current).mul_add(fraction, current).as_();
        }
    }
    channels
}

/// Writes ordered weighted sums of resolved frames into `output[..channels]`.
/// Offsets address the first channel of each frame in `samples`.
///
/// Each channel starts at `+0.0` and adds `f64(sample) * weight` in tap order,
/// without fusion or reassociation. Normalization and conversion belong to
/// the caller. No values are sanitized; the output tail stays untouched.
/// Zero channels is a no-op, regardless of the other slice shapes.
///
/// # Errors
/// [`InterpError::InvalidShape`] if nonzero channels lack output scratch,
/// offsets and weights are empty or unequal, or any frame extends outside
/// `samples` (including offset overflow). Refusal leaves all output untouched.
#[inline]
pub fn weighted_frames(
    samples: &[f32],
    offsets: &[usize],
    weights: &[f64],
    channels: usize,
    output: &mut [f64],
) -> Result<(), InterpError> {
    if channels == 0 {
        return Ok(());
    }
    let output = output
        .get_mut(..channels)
        .ok_or(InterpError::InvalidShape)?;
    let window_len = samples
        .len()
        .checked_sub(channels)
        .and_then(|last_start| last_start.checked_add(1))
        .ok_or(InterpError::InvalidShape)?;
    if offsets.is_empty() || offsets.len() != weights.len() {
        return Err(InterpError::InvalidShape);
    }
    // Equal view lengths prevent later channels from refusing after the first write.
    for (samples, total) in samples.windows(window_len).zip(output) {
        let mut sum = 0.0;
        for (&offset, &weight) in offsets.iter().zip(weights) {
            let sample = samples.get(offset).ok_or(InterpError::InvalidShape)?;
            sum += f64::from(*sample) * weight;
        }
        *total = sum;
    }
    Ok(())
}
