use std::sync::{OnceLock, atomic::Ordering};
pub use std::time::Duration;

/// `dashmap` shards, and anything else built on `parking_lot_core`, block a worker once
/// their spin budget runs out. Without the `nightly` feature that crate compiles the
/// wasm parker whose every method panics, so the first contended shard aborts the
/// worker.
use parking_lot_core as _;
use wasm_bindgen::JsCast;
use wasm_safe_thread::Builder as WasmThreadBuilder;

use crate::common::thread_id::{ACTIVE_NAMED_THREADS, thread_id_hash};
pub use crate::common::{
    thread_class::{ThreadClass, set_current_class},
    thread_id::active_named_thread_count,
};

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

/// Keeps the calling Web Worker's event loop driving async tasks and timers.
/// Call once at the start of a long-lived runtime worker: `wasm_safe_thread`
/// otherwise closes it after its spawn closure returns and tracked tasks drain,
/// killing spawned futures after one microtask drain. The registration is never
/// released because the engine worker lives for the page's lifetime.
pub fn keep_worker_alive() {
    wasm_safe_thread::task_begin();
}

pub type Thread = wasm_safe_thread::Thread;
pub type ThreadId = wasm_safe_thread::ThreadId;

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

#[derive(Default)]
pub(crate) enum GateBackend {
    #[default]
    Wasm,
}

impl GateBackend {
    #[inline]
    pub(crate) const fn posts(&self) -> u64 {
        match self {
            Self::Wasm => 0,
        }
    }

    #[inline]
    pub(crate) const fn post(&self) {
        match self {
            Self::Wasm => {}
        }
    }

    #[inline]
    pub(crate) fn park_timeout(&self, duration: Duration, _poll_posts: Option<u64>) {
        match self {
            Self::Wasm => park_timeout(duration),
        }
    }

    #[inline]
    pub(crate) fn unpark(&self, _thread_id: u64, thread: Option<&Thread>) {
        match self {
            Self::Wasm => {
                if let Some(thread) = thread {
                    unpark(thread);
                }
            }
        }
    }
}

#[inline]
pub(crate) fn gate_instant(backend: &GateBackend) -> crate::time::Instant {
    match backend {
        GateBackend::Wasm => crate::time::Instant::now(),
    }
}

#[inline]
pub fn yield_now() {}

/// The fairness yield of a loop that still has work. A no-op, as
/// [`yield_now`] is.
#[inline]
pub fn yield_runnable() {}

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

/// Uses the consumer-registered shim name (`set_wasm_shim_name`) when set, falling back to
/// `wasm_safe_thread`'s Performance-API auto-detection otherwise.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let mut builder = WasmThreadBuilder::new();
    if let Some(shim) = wasm_shim_name().get() {
        builder = builder.shim_name(shim.clone());
    }
    builder
        .spawn(f)
        .expect("BUG: WASM Worker spawn must succeed; only fails on OS resource exhaustion")
}

#[inline]
pub fn sleep(duration: Duration) {
    wasm_safe_thread::sleep(duration);
}

/// Back off a synchronous poll loop whose data is produced by another thread.
/// On wasm there is no quiescence engine, so this is a real `sleep(duration)`
/// throttle - parity with [`crate::backend::thread::paced_backoff`].
#[inline]
pub fn paced_backoff(duration: Duration) {
    sleep(duration);
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use kithara_test_utils::kithara;
    use wasm_bindgen::JsCast;
    use web_sys::DedicatedWorkerGlobalScope;

    use super::{spawn, spawn_named};
    use crate::time::{Duration, sleep};

    mod consts {
        use super::Duration;

        /// Enough workers bootstrapping at once that a browser losing one of
        /// several concurrent module loads drops a thread on most runs.
        pub(super) const WORKERS: usize = 8;
        pub(super) const TICK: Duration = Duration::from_millis(10);
        pub(super) const NAME: &str = "kithara-named-worker";
    }

    fn worker_name() -> String {
        js_sys::global()
            .unchecked_into::<DedicatedWorkerGlobalScope>()
            .name()
    }

    #[kithara::test(wasm, timeout(Duration::from_secs(10)))]
    async fn spawn_named_names_the_worker() {
        let named = spawn_named(consts::NAME, worker_name).join_async().await;
        let unnamed = spawn(worker_name).join_async().await;
        assert_eq!(named, Ok(consts::NAME.to_owned()));
        assert!(unnamed.is_ok_and(|name| name != consts::NAME));
    }

    #[kithara::test(wasm, flash(false), timeout(Duration::from_secs(10)))]
    async fn workers_spawned_together_all_start() {
        let started = Arc::new(AtomicUsize::new(0));
        for _ in 0..consts::WORKERS {
            let started = Arc::clone(&started);
            drop(spawn(move || {
                started.fetch_add(1, Ordering::Release);
            }));
        }
        while started.load(Ordering::Acquire) < consts::WORKERS {
            sleep(consts::TICK).await;
        }
    }
}
