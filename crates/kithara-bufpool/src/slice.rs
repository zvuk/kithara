use crate::pool::{Pooled, PooledOwned};

/// Wrapper for pooled Vec with tracked length.
///
/// Holds a pooled buffer and tracks the actual length of valid data,
/// which may be less than the buffer's capacity.
///
/// Useful when reading data into a pooled buffer where the amount read
/// is not known in advance.
///
/// # Example
///
/// ```
/// use kithara_bufpool::{Pool, PooledSlice};
///
/// let pool = Pool::<32, Vec<u8>>::new(1024, 128 * 1024);
/// let mut buf = pool.get_with(|b| b.resize(4096, 0));
///
/// // Simulate reading data (e.g., 100 bytes actually read)
/// let bytes_read = 100;
/// let slice = PooledSlice::new(buf, bytes_read);
///
/// assert_eq!(slice.as_slice().len(), 100);
/// ```
pub struct PooledSlice<'a, const SHARDS: usize, T> {
    buf: Pooled<'a, SHARDS, Vec<T>>,
    len: usize,
}

impl<'a, const SHARDS: usize, T> PooledSlice<'a, SHARDS, T> {
    /// Create a new `PooledSlice` from a pooled Vec and length.
    ///
    /// # Panics
    ///
    /// Panics if `len` exceeds the buffer's length.
    pub fn new(buf: Pooled<'a, SHARDS, Vec<T>>, len: usize) -> Self {
        assert!(
            len <= buf.len(),
            "PooledSlice length {} exceeds buffer length {}",
            len,
            buf.len()
        );
        Self { buf, len }
    }

    /// Get a slice view of the valid data.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// Get a mutable slice view of the valid data.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }

    /// Get the length of valid data.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume and return the inner pooled buffer.
    pub fn into_inner(self) -> Pooled<'a, SHARDS, Vec<T>> {
        self.buf
    }
}

impl<'a, const SHARDS: usize, T> AsRef<[T]> for PooledSlice<'a, SHARDS, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'a, const SHARDS: usize, T> AsMut<[T]> for PooledSlice<'a, SHARDS, T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

/// Owned wrapper for pooled Vec with tracked length (holds `Arc<Pool>`).
///
/// Like `PooledSlice` but owns an Arc to the pool, making it `'static`.
pub struct PooledSliceOwned<const SHARDS: usize, T> {
    buf: PooledOwned<SHARDS, Vec<T>>,
    len: usize,
}

impl<const SHARDS: usize, T> PooledSliceOwned<SHARDS, T> {
    /// Create a new `PooledSliceOwned` from a pooled Vec and length.
    ///
    /// # Panics
    ///
    /// Panics if `len` exceeds the buffer's length.
    pub fn new(buf: PooledOwned<SHARDS, Vec<T>>, len: usize) -> Self {
        assert!(
            len <= buf.len(),
            "PooledSliceOwned length {} exceeds buffer length {}",
            len,
            buf.len()
        );
        Self { buf, len }
    }

    /// Get a slice view of the valid data.
    pub fn as_slice(&self) -> &[T] {
        &self.buf[..self.len]
    }

    /// Get a mutable slice view of the valid data.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.buf[..self.len]
    }

    /// Get the length of valid data.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume and return the inner pooled buffer.
    pub fn into_inner(self) -> PooledOwned<SHARDS, Vec<T>> {
        self.buf
    }
}

impl<const SHARDS: usize, T> AsRef<[T]> for PooledSliceOwned<SHARDS, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<const SHARDS: usize, T> AsMut<[T]> for PooledSliceOwned<SHARDS, T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}
