//! Async yield to the executor / event loop.
//!
//! * **Native** — [`tokio::task::yield_now`].
//! * **WASM** — `wasm_safe_thread::yield_to_event_loop_async`.

#[cfg(not(target_arch = "wasm32"))]
pub async fn yield_now() {
    tokio::task::yield_now().await;
}

#[cfg(target_arch = "wasm32")]
pub async fn yield_now() {
    wasm_safe_thread::yield_to_event_loop_async().await;
}
