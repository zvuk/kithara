use super::ffi::{vDSP_vlint, vDSP_vqint};

pub fn linear_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate(source, positions, target, vDSP_vlint)
}

pub fn quadratic_interpolate_f32(source: &[f32], positions: &[f32], target: &mut [f32]) -> usize {
    interpolate(source, positions, target, vDSP_vqint)
}

type Interpolate =
    unsafe extern "C" fn(*const f32, *const f32, isize, *mut f32, isize, usize, usize);

fn interpolate(
    source: &[f32],
    positions: &[f32],
    target: &mut [f32],
    interpolate: Interpolate,
) -> usize {
    let frames = positions.len().min(target.len());
    if frames == 0 || source.is_empty() {
        return 0;
    }
    // SAFETY: source, positions, and target point at contiguous f32 buffers.
    // SAFETY: frames bounds positions and target.
    // SAFETY: source.len() is supplied as the interpolation source length.
    unsafe {
        interpolate(
            source.as_ptr(),
            positions.as_ptr(),
            1,
            target.as_mut_ptr(),
            1,
            frames,
            source.len(),
        );
    }
    frames
}
