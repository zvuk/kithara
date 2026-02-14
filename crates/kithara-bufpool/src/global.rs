use crate::pool::{PooledOwned, SharedPool};

/// Standard byte buffer pool type for the entire workspace.
///
/// Use this type everywhere instead of creating custom `SharedPool` aliases.
pub type BytePool = SharedPool<32, Vec<u8>>;

/// Standard PCM (f32) buffer pool type for the entire workspace.
///
/// Used by decoders, resamplers, and audio pipelines for zero-allocation
/// PCM sample buffer reuse.
pub type PcmPool = SharedPool<32, Vec<f32>>;

/// Pooled PCM buffer that auto-recycles to the global pool on drop.
///
/// Use this instead of `Vec<f32>` in audio pipelines to enable
/// zero-allocation buffer reuse.
pub type PcmBuf = PooledOwned<32, Vec<f32>>;

// Global byte pool (32 shards, 1024 max buffers, 64KB trim capacity)
static GLOBAL_BYTE_POOL: std::sync::OnceLock<BytePool> = std::sync::OnceLock::new();

// Global PCM pool (32 shards, 64 max buffers, 200K trim capacity)
static GLOBAL_PCM_POOL: std::sync::OnceLock<PcmPool> = std::sync::OnceLock::new();

/// Get global byte buffer pool for the entire workspace.
///
/// Lazily initialized on first call. Use this instead of creating
/// separate `BytePool` instances.
pub fn byte_pool() -> &'static BytePool {
    GLOBAL_BYTE_POOL.get_or_init(|| BytePool::new(1024, 64 * 1024))
}

/// Get global PCM buffer pool for the entire workspace.
///
/// Lazily initialized on first call. Use this instead of creating
/// separate `PcmPool` instances.
pub fn pcm_pool() -> &'static PcmPool {
    GLOBAL_PCM_POOL.get_or_init(|| PcmPool::new(64, 200_000))
}
