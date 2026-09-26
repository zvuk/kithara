use std::num::NonZeroUsize;

use kithara_apple::accelerate;

use super::{
    Portable,
    traits::{Backend, sealed},
};

/// Kernels on Accelerate (vDSP, BLAS); Apple targets only.
#[derive(Clone, Copy, Debug, Default)]
pub struct Accelerate;

impl sealed::Sealed for Accelerate {}

impl Backend for Accelerate {
    fn deinterleave_pair(&self, input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize {
        accelerate::deinterleave_pair_f32(input, left, right)
    }

    fn gather(&self, input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize {
        accelerate::gather_f32(input, stride, plane)
    }

    fn interleave_pair(&self, left: &[f32], right: &[f32], output: &mut [f32]) -> usize {
        accelerate::interleave_pair_f32(left, right, output)
    }

    fn sanitize(&self, samples: &mut [f32]) {
        Portable::default().sanitize(samples);
    }

    fn scatter(&self, plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize {
        accelerate::scatter_f32(plane, output, stride)
    }
}
