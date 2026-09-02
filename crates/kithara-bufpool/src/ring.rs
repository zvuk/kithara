use std::ops::{Deref, DerefMut};

/// FIFO view over an owning pooled buffer.
///
/// The backing slice is fixed while the ring is live. Reading and writing only
/// move indices; retained elements are never shifted.
#[non_exhaustive]
pub struct BufferRing<B> {
    buffer: B,
    head: usize,
    len: usize,
}

impl<B> BufferRing<B> {
    /// Wrap a buffer whose readable values occupy its prefix.
    ///
    /// # Errors
    ///
    /// Returns the unchanged owner when `len` exceeds the backing slice.
    pub fn from_prefix<T>(buffer: B, len: usize) -> Result<Self, B>
    where
        B: Deref<Target = [T]> + DerefMut,
    {
        if len > buffer.len() {
            return Err(buffer);
        }
        Ok(Self {
            buffer,
            head: 0,
            len,
        })
    }

    /// Number of readable elements.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether no readable elements remain.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copy and consume the next elements without shifting the retained tail.
    /// Returns `false` without changing either side when output exceeds the
    /// readable length.
    pub fn try_pop_into<T>(&mut self, output: &mut [T]) -> bool
    where
        T: Copy,
        B: Deref<Target = [T]> + DerefMut,
    {
        if output.len() > self.len {
            return false;
        }
        if output.is_empty() {
            return true;
        }
        let output_len = output.len();
        let first = output_len.min(self.buffer.len() - self.head);
        output[..first].copy_from_slice(&self.buffer[self.head..self.head + first]);
        output[first..].copy_from_slice(&self.buffer[..output_len - first]);
        self.head = (self.head + output_len) % self.buffer.len();
        self.len -= output_len;
        if self.len == 0 {
            self.head = 0;
        }
        true
    }

    /// Copy elements into the free tail without shifting readable values.
    /// Returns `false` without changing the ring when input exceeds free space.
    pub fn try_push<T>(&mut self, input: &[T]) -> bool
    where
        T: Copy,
        B: Deref<Target = [T]> + DerefMut,
    {
        let capacity = self.buffer.len();
        if input.len() > capacity - self.len {
            return false;
        }
        if input.is_empty() {
            return true;
        }
        let tail = (self.head + self.len) % capacity;
        let first = input.len().min(capacity - tail);
        self.buffer[tail..tail + first].copy_from_slice(&input[..first]);
        self.buffer[..input.len() - first].copy_from_slice(&input[first..]);
        self.len += input.len();
        true
    }

    /// Return the owning pooled buffer.
    #[must_use]
    pub fn into_inner(self) -> B {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::BufferRing;

    #[kithara::test]
    fn wraps_without_shifting_retained_values() {
        let Err(rejected) = BufferRing::from_prefix(vec![9], 2) else {
            panic!("an oversized prefix must return its owner");
        };
        assert_eq!(rejected, [9]);
        let mut empty = BufferRing::from_prefix(Vec::<i32>::new(), 0)
            .unwrap_or_else(|_| panic!("an empty prefix fits empty storage"));
        assert!(empty.try_pop_into(&mut []));
        assert!(empty.try_push(&[]));

        let mut ring = BufferRing::from_prefix(vec![1, 2, 3, 0], 3)
            .unwrap_or_else(|_| panic!("test prefix fits the backing buffer"));
        let mut oversized = [9; 4];
        assert!(!ring.try_pop_into(&mut oversized));
        assert_eq!(oversized, [9; 4]);
        let mut first = [0; 2];
        assert!(ring.try_pop_into(&mut first));
        assert_eq!(first, [1, 2]);
        assert!(!ring.try_push(&[6, 7, 8, 9]));
        assert!(ring.try_push(&[4, 5]));

        let mut rest = [0; 3];
        assert!(ring.try_pop_into(&mut rest));
        assert_eq!(rest, [3, 4, 5]);
        assert!(ring.is_empty());
        assert_eq!(ring.into_inner(), [5, 2, 3, 4]);
    }
}
