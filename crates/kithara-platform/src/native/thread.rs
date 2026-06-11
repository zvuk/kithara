pub use std::time::Duration;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

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

/// Number of active threads spawned via [`spawn_named`].
///
/// Incremented on spawn, decremented when the thread function returns.
/// Used by thread-budget tests to count only kithara-owned threads.
pub(crate) static ACTIVE_NAMED_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Returns the number of currently active threads spawned via [`spawn_named`].
#[must_use]
pub fn active_named_thread_count() -> usize {
    ACTIVE_NAMED_THREADS.load(Ordering::Acquire)
}

/// Wrap `f` to bracket its execution with the named-thread counter —
/// increments on entry (at call site, before spawn), decrements after the
/// closure returns. Used by all [`spawn_named`] variants.
fn counted<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    ACTIVE_NAMED_THREADS.fetch_add(1, Ordering::Release);
    move || {
        let result = f();
        ACTIVE_NAMED_THREADS.fetch_sub(1, Ordering::Release);
        result
    }
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

/// Block the current thread for at least `duration` (native, non-sim).
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

/// Unpark a thread parked in [`park_timeout`] (non-sim / OS park slot).
#[inline]
pub fn unpark(t: &Thread) {
    t.unpark();
}

/// Stable `u64` hash of a [`ThreadId`]. Used both for shard indexing and (under
/// `flash`) as the engine's thread key: [`current_thread_id`] and
/// the flash `unpark`'s target derive from the SAME hasher so a park and its
/// wake agree.
#[inline]
#[must_use]
pub(crate) fn thread_id_hash(id: ThreadId) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
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
