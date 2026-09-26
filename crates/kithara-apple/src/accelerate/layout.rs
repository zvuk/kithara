use std::num::NonZeroUsize;

use super::ffi::{DspComplex, DspSplitComplex, VdspStride, cblas_scopy, vDSP_ctoz, vDSP_ztoc};

mod consts {
    use super::VdspStride;

    /// Largest element count or increment one BLAS call accepts (`i32::MAX`).
    pub(super) const BLAS_MAX: usize = 0x7FFF_FFFF;

    /// Float stride between consecutive `DspComplex` pairs in an interleaved buffer.
    pub(super) const PAIR_STRIDE: VdspStride = 2;
}

/// Interleaves `left` and `right` into `output` as `[l0, r0, l1, r1, …]`.
///
/// Writes the common prefix of the three slices and returns its length in frames.
#[must_use]
pub fn interleave_pair_f32(left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
    let frames = left.len().min(right.len()).min(output.len() / 2);
    if frames == 0 {
        return 0;
    }
    let split = DspSplitComplex {
        realp: left.as_ptr().cast_mut(),
        imagp: right.as_ptr().cast_mut(),
    };
    // SAFETY: `split` points at `frames` readable floats per plane and vDSP_ztoc only reads it.
    // SAFETY: `output` holds `2 * frames` floats written as `DspComplex` pairs at float stride 2.
    // SAFETY: `DspComplex` is two `f32` fields, so it needs only `f32` alignment.
    unsafe {
        vDSP_ztoc(
            &split,
            1,
            output.as_mut_ptr().cast::<DspComplex>(),
            consts::PAIR_STRIDE,
            frames,
        );
    }
    frames
}

/// Splits `[l0, r0, l1, r1, …]` from `input` into `left` and `right`.
///
/// Reads whole pairs of the common prefix and returns their count.
#[must_use]
pub fn deinterleave_pair_f32(input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
    let frames = (input.len() / 2).min(left.len()).min(right.len());
    if frames == 0 {
        return 0;
    }
    let split = DspSplitComplex {
        realp: left.as_mut_ptr(),
        imagp: right.as_mut_ptr(),
    };
    // SAFETY: `input` holds `2 * frames` floats read as `DspComplex` pairs at float stride 2.
    // SAFETY: `split` points at `frames` writable floats per plane.
    // SAFETY: the planes and `input` are distinct borrows, so they never overlap.
    unsafe {
        vDSP_ctoz(
            input.as_ptr().cast::<DspComplex>(),
            consts::PAIR_STRIDE,
            &split,
            1,
            frames,
        );
    }
    frames
}

/// Writes `plane` into every `stride`-th slot of `output`, starting at slot 0.
///
/// The last frame may be partial: `output` needs `(frames - 1) * stride + 1`
/// slots. Returns the number of frames written.
#[must_use]
pub fn scatter_f32(plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
    let stride = stride.get();
    let frames = plane.len().min(output.len().div_ceil(stride));
    let Ok(increment) = i32::try_from(stride) else {
        for (slot, sample) in output.iter_mut().step_by(stride).zip(plane) {
            *slot = *sample;
        }
        return frames;
    };
    let chunk_frames = consts::BLAS_MAX / stride;
    for offset in (0..frames).step_by(chunk_frames) {
        let len = (frames - offset).min(chunk_frames);
        let Ok(count) = i32::try_from(len) else {
            break;
        };
        // SAFETY: frames `offset..offset + len` lie inside `plane`.
        // SAFETY: the last slot written, `(offset + len - 1) * stride`, is below `output.len()`
        // SAFETY: because `frames <= output.len().div_ceil(stride)`.
        unsafe {
            cblas_scopy(
                count,
                plane.as_ptr().add(offset),
                1,
                output.as_mut_ptr().add(offset * stride),
                increment,
            );
        }
    }
    frames
}

/// Reads every `stride`-th slot of `input`, starting at slot 0, into `plane`.
///
/// The last frame may be partial, as in [`scatter_f32`]. Returns the number
/// of frames read.
#[must_use]
pub fn gather_f32(input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
    let stride = stride.get();
    let frames = plane.len().min(input.len().div_ceil(stride));
    let Ok(increment) = i32::try_from(stride) else {
        for (slot, sample) in plane.iter_mut().zip(input.iter().step_by(stride)) {
            *slot = *sample;
        }
        return frames;
    };
    let chunk_frames = consts::BLAS_MAX / stride;
    for offset in (0..frames).step_by(chunk_frames) {
        let len = (frames - offset).min(chunk_frames);
        let Ok(count) = i32::try_from(len) else {
            break;
        };
        // SAFETY: the last slot read, `(offset + len - 1) * stride`, is below `input.len()`
        // SAFETY: because `frames <= input.len().div_ceil(stride)`.
        // SAFETY: frames `offset..offset + len` lie inside `plane`.
        unsafe {
            cblas_scopy(
                count,
                input.as_ptr().add(offset * stride),
                increment,
                plane.as_mut_ptr().add(offset),
                1,
            );
        }
    }
    frames
}
