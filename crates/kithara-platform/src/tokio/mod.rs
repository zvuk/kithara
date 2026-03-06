//! Cross-platform tokio — real `tokio` on native, WASM shims on `wasm32`.
//!
//! Re-exports everything from `tokio_with_wasm::alias` (which itself gates
//! per platform), with a custom `task` module for Web Worker lifecycle.
//!
//! Usage: `use kithara_platform::tokio;`

pub use tokio_with_wasm::alias::*;

pub mod task;

/// Ensure the platform task pool is initialized before browser-side tests run.
///
/// On wasm32 this eagerly touches the blocking task backend once so browser
/// tests do not pay lazy worker-pool setup in the measured path.
#[inline]
pub async fn ensure_thread_pool() {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = task::spawn_blocking(|| {}).await;
    }
}
