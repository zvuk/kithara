//! Fire-and-forget async task spawning.
//!
//! * **Native** — delegates to [`tokio::spawn`].
//! * **WASM** — delegates to [`wasm_bindgen_futures::spawn_local`].

use std::future::Future;

/// Spawn a fire-and-forget async task on the current executor.
///
/// On native, uses `tokio::spawn` (requires an active tokio runtime).
/// On WASM, uses `wasm_bindgen_futures::spawn_local`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_task<F>(f: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(f);
}

/// On WASM, uses `wasm_bindgen_futures::spawn_local`.
#[cfg(target_arch = "wasm32")]
pub fn spawn_task<F>(f: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(f);
}
