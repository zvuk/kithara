use crate::flash::ids::ThreadKey;
pub use crate::{
    common::thread_id::active_named_thread_count,
    native::thread::{
        Duration, JoinHandle, Thread, ThreadId, assert_main_thread, assert_not_main_thread,
        available_parallelism, current, current_thread_id, is_main_thread, is_worker_thread, park,
        spawn,
    },
};

/// Under `flash`, a cooperative yield must relinquish the quiescence engine:
/// a busy-poll loop spinning on `std::thread::yield_now` keeps the thread counted
/// as running, so the virtual clock can never advance past it — and a loop bounded
/// by a virtual-time deadline then livelocks (it waits for time its own spinning
/// prevents). The sim path parks the thread as a yield-waiter so the clock can
/// advance, then wakes it on the next advance to re-check. Off the sim path
/// (real-time scope) it stays a plain OS yield, so the real-time / RT worker
/// behaviour is unchanged. See `crate::flash::system::yield_until_advance`.
#[inline]
pub fn yield_now() {
    if crate::flash::flash_enabled() {
        crate::flash::system::yield_until_advance();
    } else {
        crate::native::thread::yield_now();
    }
}

/// Wrap `f` to bracket its execution with the named-thread counter and the
/// quiescence credit, both owned by a [`credit::DedicatedSlot`] reserved at
/// the call site (before spawn) and claimed by the child. The claim's
/// [`credit::Participant`] settles the exit on Drop — including an unwind
/// through a panicking `f()`, which previously leaked both the counter and a
/// `Running` pacer's `active` slot (wedging the engine). This makes
/// participant accounting intrinsic to the platform spawn — no consumer
/// registers anything. Off the sim path the credit half does not exist.
fn counted<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Snapshot the per-test ambient gate on the PARENT: thread-locals do not
    // cross `spawn`, so a flash test's spawned graph would otherwise see the
    // default `false`. The child re-establishes it for its whole lifetime.
    let ambient = crate::flash::ambient_snapshot();
    // Reserve this pacer's `active` slot NOW, on the parent, before the child is
    // scheduled — so a sibling that parks in the spawn→run gap still sees the
    // pacer counted and the virtual clock cannot jump past its warm-up. The
    // slot also owns the named-thread count; a spawn that never runs the child
    // returns both via the slot's Drop.
    let slot = crate::flash::system::credit::DedicatedSlot::reserve_named();
    move || {
        // Held for the closure's lifetime: restores the previous ambient on the
        // child thread when the closure returns (it must outlive `f()`).
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        // A DEDICATED pacer must run its WHOLE callstack in the test's flash mode,
        // not just propagate ambient. It loops on stateful waits (`Condvar`,
        // `park_timeout`) keyed on AMBIENT (virtual under a flash test) while
        // computing their deadlines from `Instant::now()`, which keys on the
        // `active` mode flag (stateless). If only ambient were set, `Instant::now()` would stay REAL
        // while the wait registers a VIRTUAL deadline — the pacer feeds a real-clock
        // deadline into the virtual scheduler, which the virtual clock instantly
        // overshoots, so the wait never blocks and the pacer spins, pinning the
        // engine's `active` count and freezing the big clock jump every flash test
        // needs. Setting `active` = ambient here (the audio worker already does
        // this via its `#[kithara::flash(true)]` run loop; this generalizes it to
        // EVERY `spawn_named` pacer — flush hub, offline render, …) keeps the
        // pacer's `Instant::now()` in the same clock domain as its waits.
        let _flash = crate::flash::enter_dynamic(true);
        crate::flash::system::credit::reset_credit();
        // A `spawn_named` thread is a DEDICATED virtual-time pacer: it is the
        // only kind of thread counted in the engine's sync `active` set (tokio
        // workers and the main thread are driven by the runtime, not by wrapped
        // waits, so counting them leaks). The claim marks this thread dedicated;
        // the participant's Drop (after `f()` returns or unwinds) settles the
        // credit and the named-thread count.
        let _participant = slot.claim_dedicated();
        f()
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
    crate::native::thread::spawn_named_uncounted(name, counted(f))
}

/// Under `flash`, a sleep registers a pure timed waiter on the quiescence
/// engine (deadline = virtual now + `duration`) and blocks off-lock until the
/// engine crosses it — collapsing to zero real wall-clock like every other
/// virtual wait, so a thread that sleeps to delay a state change cannot be raced
/// by a peer's virtual wait advancing the clock past it. Real-time scopes keep a
/// true wall-clock sleep. Unlike [`park_timeout`] a sleep has no early wake. See
/// `crate::flash::system::sleep_timed`.
#[inline]
pub fn sleep(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::sleep_timed(duration);
    } else {
        crate::native::thread::sleep(duration);
    }
}

/// Back off a synchronous poll loop whose data is produced by another
/// engine-visible thread. A bare `sleep` here would register a free virtual
/// `Timed` deadline (deadline = virtual now + `duration`) that the engine
/// services in isolation: each wake re-polls and re-sleeps, racing the virtual
/// clock far ahead of the real producer (the analysis decode loop vs the audio
/// worker fed by a real download). A deadline-less cooperative yield instead
/// relinquishes the engine and is re-woken on the next clock advance —
/// advancing in lockstep with the engine-visible producer (paced by its real
/// I/O), never inflating the clock on its own. Off the sim path it is a real
/// `sleep(duration)` throttle (no busy-spin), via the native arm.
#[inline]
pub fn paced_backoff(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::yield_until_advance();
    } else {
        crate::native::thread::sleep(duration);
    }
}

/// Under `flash`, a timed park registers an unparkable waiter on the
/// quiescence engine (deadline = virtual now + `duration`) and blocks off-lock
/// until the engine crosses that deadline OR a peer [`unpark`]s this thread.
/// The wait consumes no real wall-clock: when every participant is parked the
/// engine jumps the virtual clock to the earliest deadline. See
/// `crate::flash` and the crate CONTEXT.md.
#[inline]
pub fn park_timeout(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::park_timed_unparkable(duration, ThreadKey::of(current().id()));
    } else {
        // Real-time scope: a true wall-clock park, invisible to the engine.
        crate::native::thread::park_timeout(duration);
    }
}

/// Park onto the quiescence engine UNCONDITIONALLY (no `flash_enabled()`
/// consult), mirroring [`park_timeout`]'s flash arm. The lexical test rewriter
/// (`flash::virtual_park_timeout`) targets this so a flash test body's
/// `park_timeout` collapses onto virtual time without setting the `active`
/// mode flag.
#[inline]
pub(crate) fn park_timeout_virtual(duration: Duration) {
    crate::flash::system::park_timed_unparkable(duration, ThreadKey::of(current().id()));
}

/// Unpark a thread parked in [`park_timeout`].
///
/// Native (non-sim) / wasm: delegates to the OS/runtime `Thread::unpark`.
/// Under `flash`: the park MODE is decided by the TARGET's own thread
/// flags, which may disagree with this caller's (a no-ambient pool thread
/// parks on the real OS slot while a flash worker wakes it). A flash-ACTIVE
/// caller therefore fires BOTH slots: the engine entry (serialized with clock
/// jumps under the engine lock, or armed pending) AND the OS park slot. The
/// redundant token costs at most one spurious early return, which the std
/// park contract already permits.
#[inline]
pub fn unpark(t: &Thread) {
    if crate::flash::flash_enabled() {
        crate::flash::system::unpark(ThreadKey::of(t.id()));
    }
    crate::native::thread::unpark(t);
}
