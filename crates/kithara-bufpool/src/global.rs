use std::{
    fmt,
    ops::{Deref, DerefMut, RangeBounds},
    sync::OnceLock,
};

use crate::{
    BudgetExhausted, ByteBudget,
    budget::RegionBudget,
    pool::{PoolStats, PooledOwned, SharedPool},
};

pub(crate) const BYTE_MAX_BUFFERS: usize = usize::MAX;
pub(crate) const BYTE_TRIM_CAPACITY: usize = 0;
pub(crate) const PCM_MAX_BUFFERS: usize = 128;
pub(crate) const PCM_TRIM_CAPACITY: usize = 200_000;

/// Standard byte buffer pool type for the entire workspace.
///
/// `BytePool::default()` returns a clone of a process-wide `OnceLock`-backed
/// instance — cheap (one `Arc::clone`) and produces a singleton across the
/// program. Top-level entry points (main, FFI) build one pool here and pass
/// it down through their config structs; library code should never call
/// `BytePool::default()` itself — read the pool from injected config.
pub type BytePool = SharedPool<32, Vec<u8>>;

/// Standard PCM (f32) buffer pool type for the entire workspace.
///
/// Uses 8 shards (128 buffers / 8 = 16 per shard) for good single-thread
/// reuse without excessive cross-shard stealing. Same `Default` policy as
/// `BytePool`.
#[derive(Clone, Debug)]
pub struct PcmPool(SharedPool<8, Vec<f32>>);

impl PcmPool {
    /// Create a new shared PCM pool.
    #[must_use]
    pub fn new(max_buffers: usize, trim_capacity: usize) -> Self {
        Self(SharedPool::new(max_buffers, trim_capacity))
    }

    delegate::delegate! {
        to self.0 {
            /// Current number of tracked bytes across all live PCM buffers.
            #[must_use]
            pub fn allocated_bytes(&self) -> usize;
            /// Wrap an already charged PCM buffer for automatic recycling.
            #[must_use]
            #[expr(PcmBuf($))]
            pub fn attach(&self, value: Vec<f32>) -> PcmBuf;
            /// Get a PCM buffer from the shared pool.
            #[must_use]
            #[expr(PcmBuf($))]
            pub fn get(&self) -> PcmBuf;
            /// Return a PCM buffer to the pool for reuse.
            pub fn recycle(&self, value: Vec<f32>);
            /// Get pool hit/miss statistics.
            #[must_use]
            pub fn stats(&self) -> PoolStats;
        }
    }

    /// Get a PCM buffer with initialization.
    pub fn get_with<F>(&self, init: F) -> PcmBuf
    where
        F: FnOnce(&mut Vec<f32>),
    {
        PcmBuf(self.0.get_with(init))
    }

    /// Pre-warm the pool by creating and recycling `count` buffers.
    pub fn pre_warm<F>(&self, count: usize, init: F)
    where
        F: Fn(&mut Vec<f32>),
    {
        self.0.pre_warm(count, init);
    }

    /// Create a shared PCM pool with a byte budget limit.
    #[must_use]
    pub fn with_byte_budget(max_buffers: usize, trim_capacity: usize, budget: ByteBudget) -> Self {
        Self(SharedPool::with_byte_budget(
            max_buffers,
            trim_capacity,
            budget,
        ))
    }

    pub(crate) fn with_region_budget(
        max_buffers: usize,
        trim_capacity: usize,
        budget: RegionBudget,
    ) -> Self {
        Self(SharedPool::with_region_budget(
            max_buffers,
            trim_capacity,
            budget,
        ))
    }
}

/// Pooled PCM buffer that auto-recycles to the source pool on drop.
///
/// Use this instead of `Vec<f32>` in audio pipelines to enable
/// zero-allocation buffer reuse.
pub struct PcmBuf(PooledOwned<8, Vec<f32>>);

impl PcmBuf {
    delegate::delegate! {
        to self.0 {
            /// Return the allocated sample capacity.
            #[must_use]
            pub fn capacity(&self) -> usize;
            /// Remove all samples while retaining the allocated capacity.
            pub fn clear(&mut self);
            /// Grow the buffer to at least `min_len` samples under its byte budget.
            ///
            /// # Errors
            ///
            /// Returns [`BudgetExhausted`] if the growth exceeds the pool's byte
            /// budget or the requested capacity cannot be reserved.
            pub fn ensure_len(&mut self, min_len: usize) -> Result<(), BudgetExhausted>;
            /// Extract the PCM vector without returning it to the pool.
            #[must_use]
            pub fn into_inner(self) -> Vec<f32>;
            /// Shorten the buffer to `len` samples.
            pub fn truncate(&mut self, len: usize);
        }
    }

    /// Remove and yield the specified sample range.
    pub fn drain<R>(&mut self, range: R) -> std::vec::Drain<'_, f32>
    where
        R: RangeBounds<usize>,
    {
        self.0.drain(range)
    }
}

impl Deref for PcmBuf {
    type Target = [f32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PcmBuf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl fmt::Debug for PcmBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Default-constructed `BytePool` returns a process-wide singleton with a
/// 256 MB byte budget and no buffer-count limit (the budget is the cap).
/// Trim is disabled — buffers always grow up to their high-water mark.
impl Default for BytePool {
    fn default() -> Self {
        static GLOBAL: OnceLock<BytePool> = OnceLock::new();
        const BUDGET: ByteBudget = ByteBudget(256 * 1024 * 1024);
        GLOBAL
            .get_or_init(|| Self::with_byte_budget(BYTE_MAX_BUFFERS, BYTE_TRIM_CAPACITY, BUDGET))
            .clone()
    }
}

/// Default-constructed `PcmPool` returns a process-wide singleton with at
/// most 128 buffers and a 200 000-element trim cap.
impl Default for PcmPool {
    fn default() -> Self {
        static GLOBAL: OnceLock<PcmPool> = OnceLock::new();
        GLOBAL
            .get_or_init(|| Self::new(PCM_MAX_BUFFERS, PCM_TRIM_CAPACITY))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// The reason `pre_warm` exists: the buffers a decode asks for come back
    /// from the free list instead of allocating, and allocating is what the
    /// audio thread may not do. Measured on a pool nobody else holds, because
    /// the property is the warm's, not a moment's.
    #[kithara::test(native, flash(false))]
    fn a_warmed_pool_serves_what_it_was_warmed_for_without_allocating() {
        let pool = PcmPool::new(128, 200_000);
        let samples = 4_608 * 2;
        pool.pre_warm(8, |buffer| {
            buffer.clear();
            buffer.resize(samples, 0.0);
        });

        let misses_before = pool.stats().alloc_misses;
        let buffers: Vec<_> = (0..8)
            .map(|_| pool.get_with(|buffer| buffer.resize(samples, 0.0)))
            .collect();

        assert_eq!(pool.stats().alloc_misses, misses_before);
        drop(buffers);
    }

    /// One more than it was warmed for has to come from somewhere.
    #[kithara::test(native, flash(false))]
    fn a_warmed_pool_still_allocates_past_its_warm() {
        let pool = PcmPool::new(128, 200_000);
        let samples = 4_608 * 2;
        pool.pre_warm(2, |buffer| {
            buffer.clear();
            buffer.resize(samples, 0.0);
        });

        let misses_before = pool.stats().alloc_misses;
        let buffers: Vec<_> = (0..3)
            .map(|_| pool.get_with(|buffer| buffer.resize(samples, 0.0)))
            .collect();

        assert_eq!(pool.stats().alloc_misses, misses_before.saturating_add(1));
        drop(buffers);
    }

    /// A warm that hands back buffers too small for a decode is no warm at
    /// all: the first `resize` reallocates on the very path the pool exists to
    /// keep allocation-free.
    #[kithara::test(native, flash(false))]
    fn a_warmed_buffer_is_already_the_size_it_was_warmed_to() {
        let pool = PcmPool::new(128, 200_000);
        let samples = 4_608 * 2;
        pool.pre_warm(1, |buffer| {
            buffer.clear();
            buffer.resize(samples, 0.0);
        });

        let buffer = pool.get_with(|buffer| buffer.clear());

        assert!(buffer.capacity() >= samples, "{}", buffer.capacity());
    }
}
