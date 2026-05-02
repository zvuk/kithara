use std::sync::OnceLock;

use crate::pool::{PooledOwned, SharedPool};

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
pub type PcmPool = SharedPool<8, Vec<f32>>;

/// Pooled PCM buffer that auto-recycles to the source pool on drop.
///
/// Use this instead of `Vec<f32>` in audio pipelines to enable
/// zero-allocation buffer reuse.
pub type PcmBuf = PooledOwned<8, Vec<f32>>;

/// Default-constructed `BytePool` returns a process-wide singleton with a
/// 256 MB byte budget and no buffer-count limit (the budget is the cap).
/// Trim is disabled — buffers always grow up to their high-water mark.
impl Default for BytePool {
    fn default() -> Self {
        static GLOBAL: OnceLock<BytePool> = OnceLock::new();
        const BUDGET: usize = 256 * 1024 * 1024;
        GLOBAL
            .get_or_init(|| Self::with_byte_budget(usize::MAX, 0, BUDGET))
            .clone()
    }
}

/// Default-constructed `PcmPool` returns a process-wide singleton with at
/// most 128 buffers and a 200 000-element trim cap.
impl Default for PcmPool {
    fn default() -> Self {
        static GLOBAL: OnceLock<PcmPool> = OnceLock::new();
        const MAX_BUFFERS: usize = 128;
        const TRIM_CAPACITY: usize = 200_000;
        GLOBAL
            .get_or_init(|| Self::new(MAX_BUFFERS, TRIM_CAPACITY))
            .clone()
    }
}
