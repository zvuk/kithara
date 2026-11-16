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
#[inline(never)]
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

/// Writes the weighted sums of `weights.len()` consecutive frames, `stride`
/// samples apart and starting at `samples[0]`, into `output[..channels]`, with
/// the order and refusal contract of [`weighted_frames`] for offsets
/// `0, stride, 2 * stride, ...`.
///
/// # Errors
/// [`InterpError::InvalidShape`] if nonzero channels lack output scratch,
/// `weights` is empty, `channels` exceeds `stride`, or the frames extend
/// outside `samples`.
#[inline(never)]
pub fn weighted_run(
    samples: &[f32],
    weights: &[f64],
    stride: usize,
    channels: usize,
    output: &mut [f64],
) -> Result<(), InterpError> {
    if channels == 0 {
        return Ok(());
    }
    let output = output
        .get_mut(..channels)
        .ok_or(InterpError::InvalidShape)?;
    if channels > stride || weights.is_empty() {
        return Err(InterpError::InvalidShape);
    }
    let len = weights
        .len()
        .checked_sub(1)
        .and_then(|taps| taps.checked_mul(stride))
        .and_then(|start| start.checked_add(channels))
        .ok_or(InterpError::InvalidShape)?;
    let frames = samples.get(..len).ok_or(InterpError::InvalidShape)?;
    match (stride, channels) {
        (1, 1) => weighted_run_sums::<1>(frames, weights, output),
        (2, 2) => weighted_run_sums::<2>(frames, weights, output),
        (4, 4) => weighted_run_sums::<4>(frames, weights, output),
        _ => {
            for (channel, total) in output.iter_mut().enumerate() {
                let mut sum = 0.0;
                for (sample, &weight) in frames.iter().skip(channel).step_by(stride).zip(weights) {
                    sum += f64::from(*sample) * weight;
                }
                *total = sum;
            }
        }
    }
    Ok(())
}

fn weighted_run_sums<const CHANNELS: usize>(frames: &[f32], weights: &[f64], output: &mut [f64]) {
    let mut sums = [0.0_f64; CHANNELS];
    for (frame, &weight) in frames.as_chunks::<CHANNELS>().0.iter().zip(weights) {
        for (sum, &sample) in sums.iter_mut().zip(frame) {
            *sum += f64::from(sample) * weight;
        }
    }
    output.copy_from_slice(&sums);
}
