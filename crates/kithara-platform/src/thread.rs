//! Thread-like primitives for sync code.
//!
//! Delegates to platform-optimal backends:
//! * **Native** — `std::thread` (OS threads).
//! * **WASM** — `wasm_safe_thread` (Web Workers).

pub use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

#[cfg(not(target_arch = "wasm32"))]
pub type Thread = std::thread::Thread;

#[cfg(target_arch = "wasm32")]
pub type Thread = wasm_safe_thread::Thread;

#[cfg(not(target_arch = "wasm32"))]
pub type ThreadId = std::thread::ThreadId;

#[cfg(target_arch = "wasm32")]
pub type ThreadId = wasm_safe_thread::ThreadId;

#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub fn yield_now() {
    std::thread::yield_now();
}

#[cfg(target_arch = "wasm32")]
#[inline]
pub fn yield_now() {
    // No-op on WASM: Web Workers are preemptively scheduled by the OS,
    // and backpressure via ringbuf already throttles the decode loop.
    // The original `Atomics.wait(0.001ms)` FFI call on every decode
    // frame added unnecessary latency causing audio stuttering.
}

/// Returns `true` when running inside a Web Worker.
#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .is_ok()
}

/// Returns `true` when running on the browser main thread.
#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    !is_worker_thread()
}

/// Returns `false` on native targets.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    false
}

/// Returns `true` on native targets.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    true
}

/// Panic if called from a non-main thread on wasm32.
#[inline]
pub fn assert_main_thread(_label: &str) {
    #[cfg(target_arch = "wasm32")]
    if !is_main_thread() {
        panic!("main-thread-only call executed on worker thread: {_label}");
    }
}

/// Panic if called from the wasm main thread.
#[inline]
pub fn assert_not_main_thread(_label: &str) {
    #[cfg(target_arch = "wasm32")]
    if is_main_thread() {
        panic!("worker-thread-only call executed on main thread: {_label}");
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub type JoinHandle<T> = std::thread::JoinHandle<T>;

#[cfg(target_arch = "wasm32")]
pub type JoinHandle<T> = wasm_safe_thread::JoinHandle<T>;

/// Get a handle to the current thread.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn current() -> Thread {
    std::thread::current()
}

/// Get a handle to the current thread.
#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn current() -> Thread {
    wasm_safe_thread::current()
}

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
        .spawn(move || {
            // Each WASM Worker has its own module instance with separate globals.
            // Install panic hook on every thread so panics produce readable
            // messages instead of bare `RuntimeError: unreachable`.
            console_error_panic_hook::set_once();
            f()
        })
        .expect("failed to spawn thread")
}

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

/// Block until the current thread is explicitly unparked.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub fn park() {
    std::thread::park();
}

/// Block until the current thread is explicitly unparked.
#[cfg(target_arch = "wasm32")]
#[inline]
pub fn park() {
    wasm_safe_thread::park();
}

/// Block until unparked or until `duration` elapses.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub fn park_timeout(duration: Duration) {
    std::thread::park_timeout(duration);
}

/// Block until unparked or until `duration` elapses.
#[cfg(target_arch = "wasm32")]
#[inline]
pub fn park_timeout(duration: Duration) {
    wasm_safe_thread::park_timeout(duration);
}

/// Hash of the current thread's ID, usable for shard indexing.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
#[must_use]
pub fn current_thread_id() -> u64 {
    use std::hash::{Hash, Hasher};
    let id = current().id();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(target_arch = "wasm32")]
#[inline]
#[must_use]
pub fn current_thread_id() -> u64 {
    use std::hash::{Hash, Hasher};
    let id = current().id();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn native_thread_detectors_are_consistent() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(is_main_thread());
            assert!(!is_worker_thread());
            assert_main_thread("native-main");
            assert_not_main_thread("native-main");
        }
    }

    #[test]
    fn park_timeout_returns_after_unpark() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let parked = current();
            let start = Instant::now();
            let join = spawn(move || {
                sleep(Duration::from_millis(5));
                parked.unpark();
            });
            park_timeout(Duration::from_secs(1));
            join.join().expect("wake helper thread");
            assert!(start.elapsed() < Duration::from_millis(250));
        }
    }
}
