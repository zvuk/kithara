use super::OwnedBuffer;
use crate::PoolError;

/// Pooled bytes returned to their typed pool on drop.
///
/// Capacity growth is available only through checked methods. Raw `Vec`
/// growth and extraction are intentionally unavailable:
///
/// ```compile_fail
/// use kithara_bufpool::ByteBuffer;
///
/// fn unchecked_growth(mut buffer: ByteBuffer) {
///     buffer.resize(1024, 0);
/// }
/// ```
///
/// ```compile_fail
/// use kithara_bufpool::ByteBuffer;
///
/// fn detach(buffer: ByteBuffer) {
///     let _ = buffer.into_inner();
/// }
/// ```
pub struct ByteBuffer(pub(super) OwnedBuffer<32, Vec<u8>, false>);

impl ByteBuffer {
    pub(crate) fn new(inner: OwnedBuffer<32, Vec<u8>, false>) -> Self {
        Self(inner)
    }

    delegate::delegate! {
        to self.0 {
            /// Return the allocated element capacity.
            #[must_use]
            pub fn capacity(&self) -> usize;
            /// Remove every byte while retaining capacity.
            pub fn clear(&mut self);
            /// Grow to at least `min_len` zeroed bytes under both hard budgets.
            ///
            /// # Errors
            ///
            /// Returns an error when the requested capacity overflows, exceeds either
            /// hard budget, or cannot be allocated.
            pub fn ensure_len(&mut self, min_len: usize) -> Result<(), PoolError>;
            /// Return the held allocation and continue with another empty guard.
            pub fn renew(&mut self);
            /// Clear this guard and apply its configured retention policy in place.
            pub fn normalize(&mut self);
            /// Shorten the buffer without changing its capacity.
            pub fn truncate(&mut self, len: usize);
            /// Append bytes under both hard budgets.
            ///
            /// # Errors
            ///
            /// Returns an error when the resulting capacity overflows, exceeds either
            /// hard budget, or cannot be allocated.
            pub fn try_extend_from_slice(&mut self, values: &[u8]) -> Result<(), PoolError>;
        }
    }
}
