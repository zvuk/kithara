use std::panic::Location;

use crate::flash::ids::ThreadKey;
pub use crate::{
    backend::thread::{
        Duration, JoinHandle, Thread, ThreadId, assert_main_thread, assert_not_main_thread,
        available_parallelism, current, current_thread_id, is_main_thread, is_worker_thread, park,
    },
    common::thread_id::active_named_thread_count,
};

pub(crate) enum GateBackend {
    Engine,
    Native,
}

impl Default for GateBackend {
    fn default() -> Self {
        if crate::flash::flash_ambient() {
            Self::Engine
        } else {
            Self::Native
        }
    }
}

impl GateBackend {
    /// A gate park is a backstop under the gate's edge: the signal releases it, while the timeout
    /// only guards a missed edge from wedging the waiter, at the cost of one clock hop per poll
    /// interval.
    #[inline]
    pub(crate) fn park_timeout(&self, duration: Duration) {
        match self {
            Self::Engine => {
                let key = ThreadKey::of(current().id());
                crate::flash::system::park_timed_unparkable(
                    duration,
                    key,
                    crate::flash::system::ParkRole::Backstop,
                );
            }
            Self::Native => crate::backend::thread::park_timeout(duration),
        }
    }

    #[inline]
    pub(crate) fn unpark(&self, thread_id: u64, thread: Option<&Thread>) {
        match self {
            Self::Engine => {
                let key = ThreadKey::from(thread_id);
                crate::flash::system::unpark(key);
            }
            Self::Native => {
                if let Some(thread) = thread {
                    crate::backend::thread::unpark(thread);
                }
            }
        }
    }
}

#[inline]
pub(crate) fn gate_instant(backend: &GateBackend) -> crate::flash::Instant {
    match backend {
        GateBackend::Engine => crate::flash::Instant::now_virtual(),
        GateBackend::Native => crate::flash::Instant::now_real(),
    }
}

fn propagated<F, T>(f: F, origin: &'static Location<'static>) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let ambient = crate::flash::ambient_snapshot();
    let active = crate::flash::flash_enabled();
    let slot = ambient.then(|| crate::flash::system::credit::DedicatedSlot::reserve(origin));
    move || {
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        let _flash = crate::flash::enter_dynamic(active);
        let _participant = slot.map(|slot| slot.claim_thread());
        f()
    }
}

#[track_caller]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    crate::backend::thread::spawn(propagated(f, Location::caller()))
}

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
        crate::backend::thread::yield_now();
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
fn counted<F, T>(f: F, origin: &'static Location<'static>) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let ambient = crate::flash::ambient_snapshot();
    let slot = crate::flash::system::credit::DedicatedSlot::reserve_named(origin);
    move || {
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        let _flash = crate::flash::enter_dynamic(true);
        crate::flash::system::credit::reset_credit();
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
#[track_caller]
pub fn spawn_named<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    crate::backend::thread::spawn_named_uncounted(name, counted(f, Location::caller()))
}

/// Under `flash`, a sleep registers a pure timed waiter on the quiescence
/// engine (deadline = virtual now + `duration`) and blocks off-lock until the
/// engine crosses it — collapsing to zero real wall-clock like every other
/// virtual wait, so a thread that sleeps to delay a state change cannot be raced
/// by a peer's virtual wait advancing the clock past it. Real-time scopes keep a
/// true wall-clock sleep. Unlike [`park_timeout`] a sleep has no early wake. See
/// `crate::flash::system::sleep_timed`.
#[inline]
#[track_caller]
pub fn sleep(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::sleep_timed(duration);
    } else {
        crate::backend::thread::sleep(duration);
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
#[track_caller]
pub fn paced_backoff(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::yield_until_advance();
    } else {
        crate::backend::thread::sleep(duration);
    }
}

/// Under `flash`, a timed park registers an unparkable waiter on the quiescence engine
/// (deadline = virtual now + `duration`) and blocks off-lock until the engine crosses
/// that deadline OR a peer [`unpark`]s this thread. The wait consumes no real
/// wall-clock: when every participant is parked the engine jumps the virtual clock to
/// the earliest deadline.
///
/// Outside `flash`, this is a true wall-clock park, invisible to the quiescence engine.
#[inline]
#[track_caller]
pub fn park_timeout(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::system::park_timed_unparkable(
            duration,
            ThreadKey::of(current().id()),
            crate::flash::system::ParkRole::Deadline,
        );
    } else {
        crate::backend::thread::park_timeout(duration);
    }
}

/// Park onto the quiescence engine UNCONDITIONALLY (no `flash_enabled()`
/// consult), mirroring [`park_timeout`]'s flash arm. The lexical test rewriter
/// (`flash::virtual_park_timeout`) targets this so a flash test body's
/// `park_timeout` collapses onto virtual time without setting the `active`
/// mode flag.
#[inline]
pub(crate) fn park_timeout_virtual(duration: Duration) {
    crate::flash::system::park_timed_unparkable(
        duration,
        ThreadKey::of(current().id()),
        crate::flash::system::ParkRole::Deadline,
    );
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
    crate::backend::thread::unpark(t);
}
