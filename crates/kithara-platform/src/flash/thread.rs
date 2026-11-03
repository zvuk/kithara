use std::panic::Location;

pub use crate::{
    backend::thread::{
        Duration, Thread, ThreadId, assert_main_thread, assert_not_main_thread,
        available_parallelism, current, current_thread_id, is_main_thread, is_worker_thread, park,
    },
    common::{
        thread_class::{ThreadClass, set_current_class},
        thread_id::active_named_thread_count,
    },
};
use crate::{
    flash::{
        ids::ThreadKey,
        join::Join,
        sync::{Condvar, Mutex},
    },
    sync::Arc,
};

/// Handle of a platform-spawned thread. Joining waits for the thread's exit on
/// a flash condvar, so under flash the joiner parks on the quiescence engine
/// and the virtual clock keeps serving the thread it waits for.
#[derive(derive_more::Debug)]
pub struct JoinHandle<T> {
    native: crate::backend::thread::JoinHandle<T>,
    #[debug(skip)]
    exit: Arc<Exit>,
}

#[derive(Default)]
struct Exit {
    done: Mutex<bool>,
    finished: Condvar,
}

/// Owned by the spawned closure; publishes the exit on drop, unwinding included,
/// while the thread still holds its participant credit.
struct ExitSignal(Arc<Exit>);

impl Drop for ExitSignal {
    fn drop(&mut self) {
        *self.0.done.lock() = true;
        self.0.finished.notify_all();
    }
}

impl<T> JoinHandle<T> {
    /// Wait for the thread to finish and take its result.
    ///
    /// # Errors
    ///
    /// Returns the panic payload if the thread panicked.
    pub fn join(self) -> std::thread::Result<T> {
        let mut done = self.exit.done.lock();
        while !*done {
            done = self.exit.finished.wait(done);
        }
        drop(done);
        self.native.join()
    }

    /// Whether the thread's closure has returned. This reads the exit that
    /// [`Self::join`] waits on, published while the thread still holds its
    /// participant credit: the OS thread ends later, after that credit is
    /// released, so polling its end under flash lets the virtual clock run
    /// ahead of a thread that has not finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        *self.exit.done.lock()
    }

    delegate::delegate! {
        to self.native {
            #[must_use]
            pub fn thread(&self) -> &Thread;
        }
    }
}

/// Wrap `f` to publish its exit to the [`Exit`] its [`JoinHandle`] waits on.
fn signalling<F, T>(f: F) -> (impl FnOnce() -> T + Send + 'static, Arc<Exit>)
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let exit = Arc::new(Exit::default());
    let signal = ExitSignal(Arc::clone(&exit));
    let work = move || {
        let _signal = signal;
        f()
    };
    (work, exit)
}

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
    /// Deferred edges posted so far under the engine, read before an edge
    /// check so a poll parked after it owes a stop for any later post.
    #[inline]
    pub(crate) fn posts(&self) -> u64 {
        match self {
            Self::Engine => crate::flash::system::posts(),
            Self::Native => 0,
        }
    }

    /// Count a deferred edge for the engine's poll stops. Lock-free.
    #[inline]
    pub(crate) fn post(&self) {
        match self {
            Self::Engine => crate::flash::system::post(),
            Self::Native => {}
        }
    }

    /// A gate park is a backstop under the gate's edge: the signal releases it,
    /// while the timeout only guards a missed edge from wedging the waiter. A
    /// poll (`poll_posts`, the [`Self::posts`] count read before its edge check)
    /// also observes deferred edges, so it owes the clock a stop once one is
    /// posted after that read.
    #[inline]
    pub(crate) fn park_timeout(&self, duration: Duration, poll_posts: Option<u64>) {
        match self {
            Self::Engine => {
                let key = ThreadKey::of(current().id());
                crate::flash::system::park_timed_unparkable(
                    duration,
                    key,
                    poll_posts.map_or(
                        crate::flash::system::ParkRole::Backstop,
                        crate::flash::system::ParkRole::Poll,
                    ),
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

/// Capture platform context before queueing work onto a reusable thread pool.
///
/// Ambient work reserves quiescence credit while queued and running. The closure
/// restores the pool thread's context on completion, including unwinding.
#[track_caller]
pub fn wrap_pool_task<F, R>(f: F) -> impl FnOnce() -> R + Send + 'static
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    joined_pool_task(f, None)
}

/// [`wrap_pool_task`] for a closure whose `JoinHandle` shares `join`: the
/// closure's exit hands its credit there.
#[track_caller]
pub(crate) fn joined_pool_task<F, R>(
    f: F,
    join: Option<Arc<Join>>,
) -> impl FnOnce() -> R + Send + 'static
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    use crate::flash::system::credit::{self, DedicatedSlot, PoolParticipant};

    let origin = Location::caller();
    let ambient = crate::flash::ambient_snapshot();
    let slot = ambient.then(|| DedicatedSlot::reserve(origin).joined(join.clone()));
    move || {
        let _ambient = crate::flash::set_ambient_for_spawn(ambient);
        let _flash = crate::flash::enter_dynamic(false);
        credit::reset_credit();
        if let Some(slot) = slot {
            let _pacer = slot.claim_pooled();
            f()
        } else {
            let _exit = PoolParticipant::unreserved(join);
            f()
        }
    }
}

#[track_caller]
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (work, exit) = signalling(f);
    let native = crate::backend::thread::spawn(propagated(work, Location::caller()));
    JoinHandle { native, exit }
}

/// Cooperatively yields by engine-parking flash or dedicated participants until
/// the next clock advance; real-time scopes otherwise use an OS yield.
/// OS yielding leaves running credit held, so virtual-deadline busy loops would
/// freeze the clock they need to advance. Dedicated pooled closures hold that
/// credit even without an active flash region; `flash_enabled()` alone would
/// select the OS path for the thread capable of freezing the engine.
#[inline]
pub fn yield_now() {
    if yields_to_engine() {
        crate::flash::system::yield_until_advance();
    } else {
        crate::backend::thread::yield_now();
    }
}

/// Give other threads the CPU while this one stays runnable.
///
/// The fairness yield of a loop that still has work. Unlike [`yield_now`], it
/// never parks on the quiescence engine: the thread stays a counted
/// participant, so the virtual clock cannot advance past the work it holds. A
/// loop that waits for another participant yields with [`yield_now`] or
/// [`paced_backoff`] instead.
#[inline]
pub fn yield_runnable() {
    crate::backend::thread::yield_now();
}

/// Whether a cooperative wait on this thread yields to the quiescence engine.
///
/// A DEDICATED participant does even where the callstack itself is not a flash
/// region. Its credit is what holds the clock still, and
/// [`credit::DedicatedSlot::claim_pooled`](crate::flash::system::credit) states
/// the term it is held on: an engine park releases it. A pooled
/// `spawn_blocking` closure inherits the ambient gate and the credit but never
/// pushes an active region, so `flash_enabled()` alone would hand the one
/// thread that can freeze the engine the one wait that cannot thaw it.
fn yields_to_engine() -> bool {
    crate::flash::flash_enabled() || crate::flash::ctx::dedicated()
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
    let (work, exit) = signalling(f);
    let native =
        crate::backend::thread::spawn_named_uncounted(name, counted(work, Location::caller()));
    JoinHandle { native, exit }
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

/// Back off a synchronous poll loop fed by another engine-visible thread.
/// A virtual timed sleep would repeatedly advance the clock ahead of that
/// producer. Cooperative yield instead relinquishes the engine until its next
/// advance, paced by real I/O, without adding a deadline.
/// Dedicated participants take this simulated path as [`yield_now`] does.
/// Async code backs off inside a pooled `spawn_blocking` closure; sleeping
/// there would hold the clock throughout the backoff.
/// The native arm uses `sleep(duration)` to throttle without spinning.
#[inline]
#[track_caller]
pub fn paced_backoff(duration: Duration) {
    if yields_to_engine() {
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
