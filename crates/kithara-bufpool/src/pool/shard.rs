//! Per-shard storage for the sharded `Pool`.

use super::reuse::Reuse;

/// A single shard in the pool.
pub(super) struct PoolShard<T> {
    /// Buffers available for reuse.
    buffers: Vec<T>,
    max_buffers: usize,
    trim_capacity: usize,
}

impl<T> PoolShard<T>
where
    T: Reuse,
{
    pub(super) fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        /// Initial pre-allocation capacity per shard.
        const SHARD_INITIAL_CAPACITY: usize = 16;

        Self {
            max_buffers,
            trim_capacity,
            buffers: Vec::with_capacity(max_buffers.min(SHARD_INITIAL_CAPACITY)),
        }
    }

    /// Try to get a buffer from this shard.
    pub(super) fn try_get(&mut self) -> Option<T> {
        self.buffers.pop()
    }

    /// Try to return a buffer to this shard.
    pub(super) fn try_put(&mut self, mut value: T) -> bool {
        if self.buffers.len() >= self.max_buffers {
            return false; // Shard full, drop the buffer
        }

        if value.reuse(self.trim_capacity) {
            self.buffers.push(value);
            true
        } else {
            false // Capacity dropped to zero, don't reuse
        }
    }
}
