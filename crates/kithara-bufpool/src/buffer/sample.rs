use std::ops::RangeBounds;

use super::OwnedBuffer;
use crate::PoolError;

/// Pooled decoded samples returned to their typed pool on drop.
pub struct SampleBuffer(pub(super) OwnedBuffer<8, Vec<f32>, false>);

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
            /// Grow to at least `min_len` zeroed samples under both hard budgets.
            ///
            /// # Errors
            ///
            /// Returns an error when the requested capacity overflows, exceeds either
            /// hard budget, or cannot be allocated.
            pub fn ensure_len(&mut self, min_len: usize) -> Result<(), PoolError>;
            /// Reduce retained capacity to the current length and release its pool charge.
            pub fn shrink_to_fit(&mut self);
            /// Shorten the buffer without changing its capacity.
            pub fn truncate(&mut self, len: usize);
            /// Append samples under both hard budgets.
            ///
            /// # Errors
            ///
            /// Returns an error when the resulting capacity overflows, exceeds either
            /// hard budget, or cannot be allocated.
            pub fn try_extend_from_slice(&mut self, values: &[f32]) -> Result<(), PoolError>;
        }
    }
}
