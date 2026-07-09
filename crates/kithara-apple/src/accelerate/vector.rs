use std::ptr;

use super::ffi::{cblas_scopy, vDSP_vclr, vDSP_vramp};

pub fn copy_f32(source: &[f32], target: &mut [f32]) -> usize {
    let frames = source.len().min(target.len());
    if frames == 0 {
        return 0;
    }
    for offset in (0..frames).step_by(i32::MAX as usize) {
        let len = frames.saturating_sub(offset).min(i32::MAX as usize);
        let Ok(len_i32) = i32::try_from(len) else {
            break;
        };
        // SAFETY: offset and len are bounded by source and target lengths.
        // SAFETY: source and target strides are one element.
        unsafe {
            cblas_scopy(
                len_i32,
                source.as_ptr().add(offset),
                1,
                target.as_mut_ptr().add(offset),
                1,
            );
        }
    }
    frames
}

pub fn clear_f32(target: &mut [f32]) {
    if target.is_empty() {
        return;
    }
    // SAFETY: target points at target.len() contiguous f32 values with stride one.
    unsafe {
        vDSP_vclr(target.as_mut_ptr(), 1, target.len());
    }
}

pub fn ramp_f32(start: f32, step: f32, target: &mut [f32]) {
    if target.is_empty() {
        return;
    }
    let start_ptr = ptr::from_ref(&start);
    let step_ptr = ptr::from_ref(&step);
    // SAFETY: target points at target.len() contiguous f32 values.
    // SAFETY: scalar inputs are valid for the synchronous call.
    unsafe {
        vDSP_vramp(start_ptr, step_ptr, target.as_mut_ptr(), 1, target.len());
    }
}
