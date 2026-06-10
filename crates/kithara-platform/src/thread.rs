#[cfg(target_arch = "wasm32")]
use std::sync::OnceLock;
pub use std::time::Duration;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_safe_thread::Builder as WasmThreadBuilder;

/// Process-wide cell for the wasm-bindgen JS shim filename (without `.js`)
/// that spawned Workers import for `initSync`. The consumer crate sets this
/// once to its own wasm-bindgen output name — `kithara-platform` cannot know
/// it, and `wasm_safe_thread`'s Performance-API auto-detection is unreliable
/// when other `.js` files (e.g. a COOP/COEP service worker) load at the same
/// base path and sort first. Function-local so it is not a bare module-level
/// static.
#[cfg(target_arch = "wasm32")]
fn wasm_shim_name() -> &'static OnceLock<String> {
    static CELL: OnceLock<String> = OnceLock::new();
    &CELL
}

/// Register the wasm-bindgen shim name used when spawning Worker threads.
/// Call once from the consumer's wasm entry point (e.g. `wasm_bindgen(start)`)
/// before any [`spawn`]. Idempotent: the first value wins.
#[cfg(target_arch = "wasm32")]
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
#[cfg(target_arch = "wasm32")]
pub fn keep_worker_alive() {
    wasm_safe_thread::task_begin();
}

#[cfg(not(target_arch = "wasm32"))]
pub type Thread = std::thread::Thread;

#[cfg(target_arch = "wasm32")]
pub type Thread = wasm_safe_thread::Thread;

#[cfg(not(target_arch = "wasm32"))]
pub type ThreadId = std::thread::ThreadId;

#[cfg(target_arch = "wasm32")]
pub type ThreadId = wasm_safe_thread::ThreadId;

#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
#[inline]
pub fn yield_now() {
    std::thread::yield_now();
}

/// Under `flash-time`, a cooperative yield must relinquish the quiescence engine:
/// a busy-poll loop spinning on `std::thread::yield_now` keeps the thread counted
/// as running, so the virtual clock can never advance past it — and a loop bounded
/// by a virtual-time deadline then livelocks (it waits for time its own spinning
/// prevents). The sim path parks the thread as a yield-waiter so the clock can
/// advance, then wakes it on the next advance to re-check. Off the sim path
/// (real-time scope) it stays a plain OS yield, so the real-time / RT worker
/// behaviour is unchanged. See `crate::time::flash::sched::yield_until_advance`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
#[inline]
pub fn yield_now() {
    if crate::time::flash::flash_enabled() {
        crate::time::flash::sched::yield_until_advance();
    } else {
        std::thread::yield_now();
    }
}

#[cfg(target_arch = "wasm32")]
#[inline]
pub fn yield_now() {}

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

/// Number of active threads spawned via [`spawn_named`].
///
/// Incremented on spawn, decremented when the thread function returns.
/// Used by thread-budget tests to count only kithara-owned threads.
static ACTIVE_NAMED_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Returns the number of currently active threads spawned via [`spawn_named`].
#[must_use]
pub fn active_named_thread_count() -> usize {
    ACTIVE_NAMED_THREADS.load(Ordering::Acquire)
}

/// Wrap `f` to bracket its execution with the named-thread counter —
/// increments on entry (at call site, before spawn), decrements after the
/// closure returns. Used by all [`spawn_named`] variants.
///
/// Under `flash-time` (native) the bracket also owns the quiescence credit: it
/// resets this thread's credit on entry (a freshly-spawned thread must start
/// `None`) and drops it on exit (a thread that woke to `Running` and returns
/// must release its `active` slot). This makes participant accounting intrinsic
/// to the platform spawn — no consumer registers anything. Off the sim path the
/// reset/exit calls do not exist.
fn counted<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    ACTIVE_NAMED_THREADS.fetch_add(1, Ordering::Release);
    // Snapshot the per-test ambient gate on the PARENT: thread-locals do not
    // cross `spawn`, so a flash test's spawned graph would otherwise see the
    // default `false`. The child re-establishes it for its whole lifetime.
    #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
    let ambient = crate::time::flash::ambient_snapshot();
    // Reserve this pacer's `active` slot NOW, on the parent, before the child is
    // scheduled — so a sibling that parks in the spawn→run gap still sees the
    // pacer counted and the virtual clock cannot jump past its warm-up. The child
    // claims this slot as `Running` in `mark_dedicated`; the first wait / exit
    // releases it. See `credit::pre_count_dedicated`.
    #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
    crate::time::flash::credit::pre_count_dedicated();
    move || {
        // Held for the closure's lifetime: restores the previous ambient on the
        // child thread when the closure returns (it must outlive `f()`).
        #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
        let _ambient = crate::time::flash::set_ambient_for_spawn(ambient);
        // A DEDICATED pacer must run its WHOLE callstack in the test's flash mode,
        // not just propagate ambient. It loops on stateful waits (`Condvar`,
        // `park_timeout`) keyed on AMBIENT (virtual under a flash test) while
        // computing their deadlines from `Instant::now()`, which keys on FLASH_ACTIVE
        // (stateless). If only ambient were set, `Instant::now()` would stay REAL
        // while the wait registers a VIRTUAL deadline — the pacer feeds a real-clock
        // deadline into the virtual scheduler, which the virtual clock instantly
        // overshoots, so the wait never blocks and the pacer spins, pinning the
        // engine's `active` count and freezing the big clock jump every flash test
        // needs. Setting FLASH_ACTIVE = ambient here (the audio worker already does
        // this via its `#[kithara::flash(true)]` run loop; this generalizes it to
        // EVERY `spawn_named` pacer — flush hub, offline render, …) keeps the
        // pacer's `Instant::now()` in the same clock domain as its waits.
        #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
        let _flash = crate::time::flash::enter_dynamic(true);
        #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
        {
            crate::time::flash::credit::reset_credit();
            // A `spawn_named` thread is a DEDICATED virtual-time pacer: it is the
            // only kind of thread counted in the engine's sync `active` set (tokio
            // workers and the main thread are driven by the runtime, not by wrapped
            // waits, so counting them leaks). See `credit::DEDICATED`.
            crate::time::flash::credit::mark_dedicated();
        }
        let result = f();
        #[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
        crate::time::flash::credit::on_participant_exit();
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
#[cfg(not(target_arch = "wasm32"))]
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

/// Spawn a new named thread (WASM variant).
///
/// # Panics
///
/// Panics if the OS refuses to create the thread.
#[cfg(target_arch = "wasm32")]
pub fn spawn_named<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let _name = name.into();
    spawn(counted(f))
}

#[cfg(target_arch = "wasm32")]
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

/// Block the current thread for at least `duration` (native, non-sim).
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
#[inline]
pub fn sleep(duration: Duration) {
    std::thread::sleep(duration);
}

/// Under `flash-time`, a sleep registers a pure timed waiter on the quiescence
/// engine (deadline = virtual now + `duration`) and blocks off-lock until the
/// engine crosses it — collapsing to zero real wall-clock like every other
/// virtual wait, so a thread that sleeps to delay a state change cannot be raced
/// by a peer's virtual wait advancing the clock past it. Real-time scopes keep a
/// true wall-clock sleep. Unlike [`park_timeout`] a sleep has no early wake. See
/// `crate::time::flash::sched::sleep_timed`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
#[inline]
pub fn sleep(duration: Duration) {
    if crate::time::flash::flash_enabled() {
        crate::time::flash::sched::sleep_timed(duration);
    } else {
        std::thread::sleep(duration);
    }
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
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
#[inline]
pub fn park_timeout(duration: Duration) {
    std::thread::park_timeout(duration);
}

/// Under `flash-time`, a timed park registers an unparkable waiter on the
/// quiescence engine (deadline = virtual now + `duration`) and blocks off-lock
/// until the engine crosses that deadline OR a peer [`unpark`]s this thread.
/// The wait consumes no real wall-clock: when every participant is parked the
/// engine jumps the virtual clock to the earliest deadline. See
/// `crate::time::flash` and the crate README.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
#[inline]
pub fn park_timeout(duration: Duration) {
    if crate::time::flash::flash_enabled() {
        crate::time::flash::sched::park_timed_unparkable(duration, current_thread_id());
    } else {
        // Real-time scope: a true wall-clock park, invisible to the engine.
        std::thread::park_timeout(duration);
    }
}

/// Park onto the quiescence engine UNCONDITIONALLY (no `flash_enabled()`
/// consult), mirroring [`park_timeout`]'s flash arm. The lexical test rewriter
/// (`time::flash_virtual_park_timeout`) targets this so a flash test body's
/// `park_timeout` collapses onto virtual time without setting `FLASH_ACTIVE`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
#[inline]
pub(crate) fn park_timeout_virtual(duration: Duration) {
    crate::time::flash::sched::park_timed_unparkable(duration, current_thread_id());
}

/// Unpark a thread parked in [`park_timeout`].
///
/// Native (non-sim) / wasm: delegates to the OS/runtime `Thread::unpark`.
/// Under `flash-time`: the park MODE is decided by the TARGET's own thread
/// flags, which may disagree with this caller's (a no-ambient pool thread
/// parks on the real OS slot while a flash worker wakes it). A flash-ACTIVE
/// caller therefore fires BOTH slots: the engine entry (serialized with clock
/// jumps under the engine lock, or armed pending) AND the OS park slot. The
/// redundant token costs at most one spurious early return, which the std
/// park contract already permits.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
#[inline]
pub fn unpark(t: &Thread) {
    if crate::time::flash::flash_enabled() {
        crate::time::flash::sched::unpark(thread_id_hash(t.id()));
    }
    t.unpark();
}

/// Unpark a thread parked in [`park_timeout`] (non-sim / OS park slot).
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
#[inline]
pub fn unpark(t: &Thread) {
    t.unpark();
}

/// Unpark a thread parked in [`park_timeout`] (wasm runtime park slot).
#[cfg(target_arch = "wasm32")]
#[inline]
pub fn unpark(t: &Thread) {
    t.unpark();
}

/// Block until unparked or until `duration` elapses.
#[cfg(target_arch = "wasm32")]
#[inline]
pub fn park_timeout(duration: Duration) {
    wasm_safe_thread::park_timeout(duration);
}

/// Stable `u64` hash of a [`ThreadId`]. Used both for shard indexing and (under
/// `flash-time`) as the engine's thread key: [`current_thread_id`] and
/// [`unpark`]'s target derive from the SAME hasher so a park and its wake agree.
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

    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn native_thread_detectors_are_consistent() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(is_main_thread());
            assert!(!is_worker_thread());
            assert_main_thread("native-main");
            assert_not_main_thread("native-main");
        }
    }

    // `flash(false)`: a real park/unpark timing test. It measures REAL
    // wall-clock with `std::time::Instant` (not the platform clock) and asserts
    // the unpark wakes within 250ms. The lexical flash rewrite would retarget
    // `Instant::now()` onto the engine `flash_virtual_now`, changing the clock
    // and leaving the `std::time::Instant` import unused, so opt out.
    #[kithara::test(flash(false))]
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
            join.join()
                .expect("BUG: wake-helper thread joined cleanly without panicking");
            assert!(start.elapsed() < Duration::from_millis(250));
        }
    }
}
