use std::num::NonZeroUsize;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use super::Accelerate;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
use super::Portable;

pub(super) mod sealed {
    pub trait Sealed {}
}

/// Vector kernels one backend implements.
///
/// Every kernel handles the common prefix of its slices and returns its
/// length in frames; nothing is sanitized unless a kernel says so.
pub trait Backend: sealed::Sealed {
    /// Splits whole `[l, r]` pairs of `input` into `left` and `right`.
    fn deinterleave_pair(&self, input: &[f32], left: &mut [f32], right: &mut [f32]) -> usize;

    /// Reads every `stride`-th slot of `input` from slot 0 into `plane`; the
    /// last frame may be partial, as in [`Backend::scatter`].
    fn gather(&self, input: &[f32], stride: NonZeroUsize, plane: &mut [f32]) -> usize;

    /// Writes `[l0, r0, l1, r1, …]` into `output`.
    fn interleave_pair(&self, left: &[f32], right: &[f32], output: &mut [f32]) -> usize;

    /// Replaces `NaN`, ±infinity, subnormals and −0.0 with `+0.0` in place.
    fn sanitize(&self, samples: &mut [f32]);

    /// Writes `plane` into every `stride`-th slot of `output` from slot 0; the
    /// last frame may be partial (`output.len().div_ceil(stride)` frames fit).
    fn scatter(&self, plane: &[f32], output: &mut [f32], stride: NonZeroUsize) -> usize;
}

/// The backend this build target uses by default.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub type Platform = Accelerate;

/// The backend this build target uses by default.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub type Platform = Portable;

#[cfg(all(test, any(target_os = "macos", target_os = "ios")))]
mod tests {
    use std::any::TypeId;

    use kithara_test_utils::kithara;

    use super::{Accelerate, Platform};

    #[kithara::test(native)]
    fn apple_builds_default_to_accelerate() {
        assert_eq!(TypeId::of::<Platform>(), TypeId::of::<Accelerate>());
    }
}
