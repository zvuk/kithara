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

/// Macro to declare a global buffer pool with lazy initialization.
///
/// Creates a static pool that is initialized on first access using `OnceLock`.
/// The pool is allocated with `Box::leak` to obtain a `'static` reference.
///
/// # Example
///
/// ```
/// use kithara_bufpool::global_pool;
///
/// // Declare a global pool for Vec<f32>
/// global_pool!(f32_pool, F32_POOL, 32, Vec<f32>, 1024, 32768);
///
/// // Access via the function name
/// let pool = f32_pool();
/// let buf = pool.get();
/// ```
///
/// # Generated Items
///
/// - Static: `$static_name`
/// - Function: `$fn_name()`
///
/// # Parameters
///
/// - `$fn_name`: Function name
/// - `$static_name`: Static variable name
/// - `$shards`: Number of shards
/// - `$type`: Type of buffer (e.g., `Vec<u8>`, `Vec<f32>`)
/// - `$max_buffers`: Maximum total buffers across all shards
/// - `$trim_capacity`: Shrink buffers to this capacity when returning to pool
#[macro_export]
macro_rules! global_pool {
    ($fn_name:ident, $static_name:ident, $shards:expr, $type:ty, $max_buffers:expr, $trim_capacity:expr) => {
        static $static_name: std::sync::OnceLock<&'static $crate::Pool<$shards, $type>> =
            std::sync::OnceLock::new();

        #[allow(dead_code)]
        fn $fn_name() -> &'static $crate::Pool<$shards, $type> {
            $static_name.get_or_init(|| {
                let pool = $crate::Pool::<$shards, $type>::new($max_buffers, $trim_capacity);
                Box::leak(Box::new(pool))
            })
        }
    };
}
