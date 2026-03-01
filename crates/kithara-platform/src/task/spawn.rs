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
pub fn spawn<F>(f: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(f);
}

/// On WASM, uses `wasm_bindgen_futures::spawn_local` with
/// `wasm_safe_thread::task_begin/task_finished` so Web Workers
/// wait for the task to complete before shutting down.
#[cfg(target_arch = "wasm32")]
pub fn spawn<F>(f: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_safe_thread::task_begin();
    wasm_bindgen_futures::spawn_local(async {
        f.await;
        wasm_safe_thread::task_finished();
    });
}
