use std::ops::RangeBounds;

use super::OwnedBuffer;
use crate::PoolError;

/// Pooled decoded samples returned to their typed pool on drop.
#[derive(derive_more::Debug)]
#[debug("{:?}", _0.value)]
pub struct SampleBuffer(pub(super) OwnedBuffer<8, Vec<f32>, false>);

impl std::ops::Deref for SampleBuffer {
    type Target = [f32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SampleBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl AsMut<[f32]> for SampleBuffer {
    fn as_mut(&mut self) -> &mut [f32] {
        self
    }
}

impl SampleBuffer {
    pub(crate) fn new(inner: OwnedBuffer<8, Vec<f32>, false>) -> Self {
        Self(inner)
    }

    /// Remove and yield a range without growing the buffer.
    pub fn drain<R>(&mut self, range: R) -> std::vec::Drain<'_, f32>
    where
        R: RangeBounds<usize>,
    {
        self.0.drain(range)
    }

    /// Retain samples matching `keep` without growing the buffer.
    pub fn retain<F>(&mut self, keep: F)
    where
        F: FnMut(&f32) -> bool,
    {
        self.0.retain(keep);
    }

    delegate::delegate! {
        to self.0 {
            /// Return the allocated element capacity.
            #[must_use]
            pub fn capacity(&self) -> usize;
            /// Remove every sample while retaining capacity.
            pub fn clear(&mut self);
            /// Remove consecutive duplicate samples without growing the buffer.
            pub fn dedup(&mut self);
            /// Grow to at least `min_len` zeroed samples under the region budget.
            ///
            /// # Errors
            ///
            /// Returns an error when the requested capacity overflows, exceeds the
            /// region budget, or cannot be allocated.
            pub fn ensure_len(&mut self, min_len: usize) -> Result<(), PoolError>;
            /// Reduce capacity toward `min_capacity`, retaining all samples and releasing its charge.
            pub fn shrink_to(&mut self, min_capacity: usize);
            /// Reduce retained capacity to the current length and release its pool charge.
            pub fn shrink_to_fit(&mut self);
            /// Shorten the buffer without changing its capacity.
            pub fn truncate(&mut self, len: usize);
            /// Reserve room for `additional` samples without changing their length.
            ///
            /// # Errors
            ///
            /// Returns an error when the resulting capacity overflows, exceeds the
            /// region budget, or cannot be allocated.
            pub fn try_reserve(&mut self, additional: usize) -> Result<(), PoolError>;
            /// Append samples under the region budget.
            ///
            /// # Errors
            ///
            /// Returns an error when the resulting capacity overflows, exceeds the
            /// region budget, or cannot be allocated.
            pub fn try_extend_from_slice(&mut self, values: &[f32]) -> Result<(), PoolError>;
        }
    }
}
