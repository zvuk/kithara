use crossbeam_queue::ArrayQueue;

use super::reuse::Reuse;

/// A single shard in the pool.
///
/// Backed by a lock-free bounded [`ArrayQueue`] so both `get` and `put`
/// are wait-free on the hot path. Capacity is fixed at construction; a full
/// queue on return drops the buffer (see [`Pool::put`](super::core::Pool::put)).
pub(super) struct PoolShard<T> {
    free: ArrayQueue<T>,
    trim_capacity: usize,
}

impl<T> PoolShard<T>
where
    T: Reuse,
{
    /// Largest per-shard free-list the pool will eagerly allocate.
    ///
    /// Pools that pass `max_buffers = usize::MAX` (count-unbounded, capped only
    /// by the byte budget) are clamped to this so the backing array stays a
    /// recycle free-list sized for the live working set, not an exabyte alloc.
    const MAX_SLOTS: usize = 1024;

    pub(super) fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        let cap = max_buffers.clamp(1, Self::MAX_SLOTS);
        Self {
            trim_capacity,
            free: ArrayQueue::new(cap),
        }
    }

    /// Try to get a buffer from this shard.
    pub(super) fn try_get(&self) -> Option<T> {
        self.free.pop()
    }

    /// Try to return a buffer to this shard.
    ///
    /// Trims via [`Reuse::reuse`] before storing. Returns `false` when the
    /// value is unfit for reuse or the queue is full; the caller drops it.
    pub(super) fn try_put(&self, mut value: T) -> bool {
        if !value.reuse(self.trim_capacity) {
            return false;
        }
        self.free.push(value).is_ok()
    }
}
