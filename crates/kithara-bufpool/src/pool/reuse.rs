//! `Reuse` trait + canonical `Vec<T>` implementation.

/// Trait for types that can be reused in a pool.
///
/// Implementors must provide logic to clear/reset the value
/// and optionally shrink capacity to a trim size.
pub trait Reuse {
    /// Returns the number of bytes this value occupies in memory.
    ///
    /// Used by the pool's byte budget tracking. Default returns 0
    /// (no budget tracking for types that don't override this).
    fn byte_size(&self) -> usize {
        0
    }

    /// Prepare this value for reuse.
    ///
    /// Should clear the contents and optionally shrink capacity
    /// to the specified trim size to prevent unbounded growth.
    ///
    /// Returns `true` if the value still has capacity and can be reused,
    /// `false` if it should be dropped.
    fn reuse(&mut self, trim: usize) -> bool;
}

/// Reuse implementation for `Vec<T>`.
///
/// Clears the vector and optionally shrinks capacity.
///
/// - `trim = 0` means "never shrink" — the buffer is always accepted
///   for reuse as long as it has capacity. This is used by [`crate::BytePool`]
///   where buffers vary in size and trimming would defeat reuse.
/// - `trim > 0` only shrinks when capacity exceeds `2 × trim`,
///   preventing realloc churn for buffers near the target size.
impl<T> Reuse for Vec<T> {
    fn byte_size(&self) -> usize {
        self.capacity() * size_of::<T>()
    }

    fn reuse(&mut self, trim: usize) -> bool {
        /// Shrink multiplier: only trim when capacity exceeds trim * `TRIM_HYSTERESIS`.
        const TRIM_HYSTERESIS: usize = 2;

        self.clear();
        if trim > 0 && self.capacity() > trim.saturating_mul(TRIM_HYSTERESIS) {
            self.shrink_to(trim);
        }
        self.capacity() > 0
    }
}
