pub use std::time::Duration;

pub use crate::common::thread_id::active_named_thread_count;
use crate::common::thread_id::{counted, thread_id_hash};

pub type Thread = std::thread::Thread;

pub type ThreadId = std::thread::ThreadId;

#[inline]
pub fn yield_now() {
    std::thread::yield_now();
}

/// Returns `false` on native targets.
#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    false
}

/// Returns `true` on native targets.
#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    true
}

/// Panic if called from a non-main thread on wasm32.
#[inline]
pub fn assert_main_thread(_label: &str) {}

/// Panic if called from the wasm main thread.
#[inline]
pub fn assert_not_main_thread(_label: &str) {}

pub type JoinHandle<T> = std::thread::JoinHandle<T>;

/// Get a handle to the current thread.
#[inline]
#[must_use]
pub fn current() -> Thread {
    std::thread::current()
}

/// Spawn a new thread.
///
/// On WASM, uses [`wasm_safe_thread::Builder`] with an explicit `shim_name`
/// so workers can locate the wasm-bindgen JS shim for `initSync`.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}

/// Spawn a new named thread.
///
/// Sets the OS thread name and tracks the thread in [`active_named_thread_count`].
/// The counter is decremented automatically when `f` returns.
///
/// # Panics
///
/// Panics if the OS refuses to create the thread.
pub fn spawn_named<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(counted(f))
        .expect(
            "BUG: spawn_named must succeed; thread::Builder only fails on OS resource exhaustion",
        )
}

/// Block the current thread for at least `duration` (real OS sleep).
#[inline]
pub fn sleep(duration: Duration) {
    std::thread::sleep(duration);
}

/// Block until the current thread is explicitly unparked.
#[inline]
pub fn park() {
    std::thread::park();
}

/// Block until unparked or until `duration` elapses.
#[inline]
pub fn park_timeout(duration: Duration) {
    std::thread::park_timeout(duration);
}

/// Unpark a thread parked in [`park_timeout`] (the OS park slot).
#[inline]
pub fn unpark(t: &Thread) {
    t.unpark();
}

/// Hash of the current thread's ID, usable for shard indexing.
#[inline]
#[must_use]
pub fn current_thread_id() -> u64 {
    thread_id_hash(current().id())
}

/// Returns the number of hardware threads available.
#[inline]
#[must_use]
pub fn available_parallelism() -> Option<std::num::NonZeroUsize> {
    std::thread::available_parallelism().ok()
}
