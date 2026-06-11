pub use std::time::Duration;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use wasm_bindgen::JsCast;
use wasm_safe_thread::Builder as WasmThreadBuilder;

/// Process-wide cell for the wasm-bindgen JS shim filename (without `.js`)
/// that spawned Workers import for `initSync`. The consumer crate sets this
/// once to its own wasm-bindgen output name — `kithara-platform` cannot know
/// it, and `wasm_safe_thread`'s Performance-API auto-detection is unreliable
/// when other `.js` files (e.g. a COOP/COEP service worker) load at the same
/// base path and sort first. Function-local so it is not a bare module-level
/// static.
fn wasm_shim_name() -> &'static OnceLock<String> {
    static CELL: OnceLock<String> = OnceLock::new();
    &CELL
}

/// Register the wasm-bindgen shim name used when spawning Worker threads.
/// Call once from the consumer's wasm entry point (e.g. `wasm_bindgen(start)`)
/// before any [`spawn`]. Idempotent: the first value wins.
pub fn set_wasm_shim_name(name: impl Into<String>) {
    let _ = wasm_shim_name().set(name.into());
}

/// Keep the calling Worker's JS event loop running for the lifetime of the
/// worker, so async tasks and timers spawned on it (via
/// [`tokio::task::spawn`](crate::tokio::task) / `setTimeout`-backed
/// [`time::sleep`](crate::time::sleep)) keep being driven.
///
/// `wasm_safe_thread` terminates a Worker once its spawn closure returns and
/// no tracked tasks remain. A worker that hosts a long-lived async runtime
/// (rather than a single blocking computation) returns from its closure
/// immediately after spawning its tasks, so without this the Worker
/// `close()`s after one microtask drain and every spawned future dies.
///
/// Call once, on the worker thread, at the top of such a worker entry point.
/// The registration is intentionally never released: the engine worker lives
/// for the page's lifetime.
pub fn keep_worker_alive() {
    wasm_safe_thread::task_begin();
}

pub type Thread = wasm_safe_thread::Thread;

pub type ThreadId = wasm_safe_thread::ThreadId;

#[inline]
pub fn yield_now() {}

/// Returns `true` when running inside a Web Worker.
#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    js_sys::global()
        .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
        .is_ok()
}

/// Returns `true` when running on the browser main thread.
#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    !is_worker_thread()
}

/// Panic if called from a non-main thread on wasm32.
#[inline]
pub fn assert_main_thread(_label: &str) {
    if !is_main_thread() {
        panic!("main-thread-only call executed on worker thread: {_label}");
    }
}

/// Panic if called from the wasm main thread.
#[inline]
pub fn assert_not_main_thread(_label: &str) {
    if is_main_thread() {
        panic!("worker-thread-only call executed on main thread: {_label}");
    }
}

pub type JoinHandle<T> = wasm_safe_thread::JoinHandle<T>;

/// Get a handle to the current thread.
#[inline]
#[must_use]
pub fn current() -> Thread {
    wasm_safe_thread::current()
}

/// Number of active threads spawned via [`spawn_named`].
///
/// Incremented on spawn, decremented when the thread function returns.
/// Used by thread-budget tests to count only kithara-owned threads.
///
/// Duplicate of `native::thread::ACTIVE_NAMED_THREADS` until 1.11b/common:
/// the native backend is gated off on wasm32, so wasm carries its own copy.
static ACTIVE_NAMED_THREADS: AtomicUsize = AtomicUsize::new(0);

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

/// Spawn a new named thread (WASM variant).
///
/// # Panics
///
/// Panics if the OS refuses to create the thread.
pub fn spawn_named<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let _name = name.into();
    spawn(counted(f))
}

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Use the consumer-registered shim name (see `set_wasm_shim_name`); fall
    // back to `wasm_safe_thread`'s Performance-API auto-detection only when
    // unset. Hardcoding here would couple this primitive crate to one
    // consumer's wasm-bindgen output filename, and a stale name silently
    // serves the SPA-fallback HTML to `initSync` so the worker never boots.
    let mut builder = WasmThreadBuilder::new();
    if let Some(shim) = wasm_shim_name().get() {
        builder = builder.shim_name(shim.clone());
    }
    builder
        .spawn(move || {
            console_error_panic_hook::set_once();
            f()
        })
        .expect("BUG: WASM Worker spawn must succeed; only fails on OS resource exhaustion")
}

#[inline]
pub fn sleep(duration: Duration) {
    wasm_safe_thread::sleep(duration);
}

/// Block until the current thread is explicitly unparked.
#[inline]
pub fn park() {
    wasm_safe_thread::park();
}

/// Block until unparked or until `duration` elapses.
#[inline]
pub fn park_timeout(duration: Duration) {
    wasm_safe_thread::park_timeout(duration);
}

/// Unpark a thread parked in [`park_timeout`] (wasm runtime park slot).
#[inline]
pub fn unpark(t: &Thread) {
    t.unpark();
}

/// Stable `u64` hash of a [`ThreadId`], usable for shard indexing.
///
/// Duplicate of `native::thread::thread_id_hash` until 1.11b/common: the
/// native backend is gated off on wasm32, so wasm carries its own copy.
#[inline]
#[must_use]
fn thread_id_hash(id: ThreadId) -> u64 {
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

#[inline]
#[must_use]
pub fn available_parallelism() -> Option<std::num::NonZeroUsize> {
    wasm_safe_thread::available_parallelism().ok()
}
