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

/// Spawn a new named thread WITHOUT the named-thread counter bracket: the
/// single `thread::Builder` site shared by [`spawn_named`] (which wraps `f`
/// in `counted`) and callers that own their own counting bracket — for those,
/// delegating to [`spawn_named`] would double-count.
///
/// # Panics
///
/// Panics if the OS refuses to create the thread.
pub(crate) fn spawn_named_uncounted<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect(
            "BUG: spawn_named must succeed; thread::Builder only fails on OS resource exhaustion",
        )
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
    spawn_named_uncounted(name, counted(f))
}

/// Block the current thread for at least `duration` (real OS sleep).
#[inline]
#[track_caller]
pub fn sleep(duration: Duration) {
    crate::no_block::forbid("thread::sleep");
    std::thread::sleep(duration);
}

/// Back off a synchronous poll loop whose data is produced by another
/// engine-visible thread (e.g. the analysis decode loop waiting on the audio
/// worker's ring). Off the sim path this is a real `sleep(duration)` throttle.
/// Under `flash` it relinquishes the quiescence engine as a deadline-less
/// cooperative yield instead, so the virtual clock is paced by the producer's
/// progress rather than inflated by a free virtual timer. See
/// `crate::flash::thread::paced_backoff`.
#[inline]
#[track_caller]
pub fn paced_backoff(duration: Duration) {
    sleep(duration);
}

/// Block until the current thread is explicitly unparked.
#[inline]
#[track_caller]
pub fn park() {
    crate::no_block::forbid("thread::park");
    std::thread::park();
}

/// Block until unparked or until `duration` elapses.
#[inline]
#[track_caller]
pub fn park_timeout(duration: Duration) {
    crate::no_block::forbid("thread::park_timeout");
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
