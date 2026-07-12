use super::ffi::{vDSP_vlint, vDSP_vqint};

pub fn linear_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate::<LinearInterpolation>(source, positions, target)
}

pub fn quadratic_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate::<QuadraticInterpolation>(source, positions, target)
}

fn interpolate<I>(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize
where
    I: Interpolation,
{
    let frames = positions.len().min(target.len());
    if frames == 0 || source.is_empty() {
        return 0;
    }
    I::interpolate(source, positions, target, frames);
    frames
}

trait Interpolation {
    fn interpolate(source: &[f32], positions: &[f32], target: &mut [f32], frames: usize);
}

struct LinearInterpolation;

impl Interpolation for LinearInterpolation {
    fn interpolate(source: &[f32], positions: &[f32], target: &mut [f32], frames: usize) {
        // SAFETY: source, positions, and target point at contiguous f32 buffers.
        // SAFETY: frames bounds positions and target.
        // SAFETY: source.len() is supplied as the interpolation source length.
        unsafe {
            vDSP_vlint(
                source.as_ptr(),
                positions.as_ptr(),
                1,
                target.as_mut_ptr(),
                1,
                frames,
                source.len(),
            );
        }
    }
}

struct QuadraticInterpolation;

impl Interpolation for QuadraticInterpolation {
    fn interpolate(source: &[f32], positions: &[f32], target: &mut [f32], frames: usize) {
        // SAFETY: source, positions, and target point at contiguous f32 buffers.
        // SAFETY: frames bounds positions and target.
        // SAFETY: source.len() is supplied as the interpolation source length.
        unsafe {
            vDSP_vqint(
                source.as_ptr(),
                positions.as_ptr(),
                1,
                target.as_mut_ptr(),
                1,
                frames,
                source.len(),
            );
        }
    }
}
