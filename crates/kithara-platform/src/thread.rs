//! Thread-like primitives for sync code.
//!
//! Delegates to [`wasm_safe_thread`] for cross-platform threading.
//! On native: uses OS threads. On WASM: uses Web Workers.

pub use std::time::Duration;

pub use wasm_safe_thread::{JoinHandle, yield_now};

/// Spawn a new thread.
///
/// On WASM, uses [`wasm_safe_thread::Builder`] with an explicit `shim_name`
/// so workers can locate the wasm-bindgen JS shim for `initSync`.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    wasm_safe_thread::spawn(f)
}

/// The wasm-bindgen JS shim name (crate name with hyphens → underscores).
/// Workers use this to locate the JS module for `initSync`.
#[cfg(target_arch = "wasm32")]
const SHIM_NAME: &str = "kithara-wasm";

#[cfg(target_arch = "wasm32")]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    wasm_safe_thread::Builder::new()
        .shim_name(SHIM_NAME.to_owned())
        .spawn(f)
        .expect("failed to spawn thread")
}

/// Blocking backoff for synchronous retry loops.
///
/// Uses [`wasm_safe_thread::sleep`] which adapts to the platform:
/// native uses `thread::sleep`, WASM uses an appropriate mechanism.
pub fn backoff(duration: Duration) {
    wasm_safe_thread::sleep(duration);
}
