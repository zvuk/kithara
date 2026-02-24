//! One-time rayon thread pool initialization for WASM tests.
//!
//! On native this is a no-op. On WASM it calls
//! [`wasm_bindgen_rayon::init_thread_pool`] to spawn Web Workers.
//! Safe to call multiple times — only the first call does anything.

#[cfg(not(target_arch = "wasm32"))]
pub async fn ensure_thread_pool() {}

#[cfg(target_arch = "wasm32")]
pub async fn ensure_thread_pool() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static INITIALIZED: AtomicBool = AtomicBool::new(false);
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    const THREAD_COUNT: usize = 2;
    wasm_bindgen_futures::JsFuture::from(wasm_bindgen_rayon::init_thread_pool(THREAD_COUNT))
        .await
        .expect("failed to initialize rayon thread pool");
}
