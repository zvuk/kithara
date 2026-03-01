//! Thread-like primitives for sync code.
//!
//! Delegates to platform-optimal backends:
//! * **Native** — `std::thread` (OS threads).
//! * **WASM** — `wasm_safe_thread` (Web Workers).

pub use std::time::Duration;

// ── yield_now ───────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub fn yield_now() {
    std::thread::yield_now();
}

#[cfg(target_arch = "wasm32")]
#[inline]
pub fn yield_now() {
    wasm_safe_thread::yield_now();
}

// ── JoinHandle ──────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub type JoinHandle<T> = std::thread::JoinHandle<T>;

#[cfg(target_arch = "wasm32")]
pub type JoinHandle<T> = wasm_safe_thread::JoinHandle<T>;

// ── spawn ───────────────────────────────────────────────────────────

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
    std::thread::spawn(f)
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

// ── sleep ────────────────────────────────────────────────────────────

/// Block the current thread for at least `duration`.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub fn sleep(duration: Duration) {
    std::thread::sleep(duration);
}

#[cfg(target_arch = "wasm32")]
#[inline]
pub fn sleep(duration: Duration) {
    wasm_safe_thread::sleep(duration);
}

// ── current_thread_id ───────────────────────────────────────────────

/// Hash of the current thread's ID, usable for shard indexing.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn current_thread_id() -> u64 {
    use std::hash::{Hash, Hasher};
    let id = std::thread::current().id();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn current_thread_id() -> u64 {
    use std::hash::{Hash, Hasher};
    let id = wasm_safe_thread::current().id();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

// ── available_parallelism ───────────────────────────────────────────

/// Returns the number of hardware threads available.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn available_parallelism() -> Option<std::num::NonZeroUsize> {
    std::thread::available_parallelism().ok()
}

#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn available_parallelism() -> Option<std::num::NonZeroUsize> {
    wasm_safe_thread::available_parallelism().ok()
}
