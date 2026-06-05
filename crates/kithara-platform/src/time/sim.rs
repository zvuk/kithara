pub use std::time::Duration;
use std::{
    cell::Cell,
    future::Future,
    ops::{Add, Sub},
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use parking_lot::Mutex;

/// Quiescence-driven virtual-clock engine. Submodule of `sim`, which is already
/// gated on `feature = "sim-time"` + native, so it needs no extra feature gate.
/// The engine drives `SIM_NANOS` forward at quiescent points. Its consumers are
/// the platform wait primitives (`thread::park_timeout`, `sync::Condvar`,
/// async `SimSleep`/`Notify`) plus the harness, so it compiles whenever
/// `sim-time` is on. The engine API stays `pub(crate)`.
pub mod sched;

/// Engine-backed `sleep` future: registers a virtual deadline + the task waker
/// on the quiescence engine on its first poll, then resolves once the engine
/// crosses that deadline. Collapses to zero wall-clock (the clock jumps when all
/// participants park). Resolution is GRANT-driven (`handle.granted()`), never a
/// bare clock check; the task's `active_async` slot is owned by the spawn
/// poll-wrapper gate ([`Participating`]), so this future touches no counter.
pub struct SimSleep {
    delta_nanos: u64,
    handle: Option<sched::AsyncHandle>,
}

impl SimSleep {
    pub(crate) fn new(duration: Duration) -> Self {
        Self {
            delta_nanos: duration_to_nanos(duration),
            handle: None,
        }
    }
}

impl Future for SimSleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(handle) = self.handle.as_ref() {
            if handle.granted() {
                // The engine crossed our deadline and granted this waiter.
                // Resolve is GRANT-driven, never a bare `SIM_NANOS >= deadline`
                // clock check: only the engine firing THIS waiter sets `granted`,
                // so a clock that jumps past our deadline via some OTHER advance
                // cannot resolve us early. The task's `active_async` count is
                // owned by the spawn poll-wrapper, so resolve touches no counter.
                self.handle = None;
                return Poll::Ready(());
            }
            // Spurious re-poll before the engine fires us: stay parked.
            return Poll::Pending;
        }
        // First poll: register `delta` from the current virtual instant; the
        // deadline is computed under the engine lock (no backward-clock race).
        let (handle, adv) = sched::register_sleep_async(self.delta_nanos, cx.waker().clone());
        self.handle = Some(handle);
        sched::fire_advance(adv);
        Poll::Pending
    }
}

impl Drop for SimSleep {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            sched::cancel_async_wait(handle);
        }
    }
}

/// Per-task quiescence state, tracked by the [`Participating`] gate. The task
/// occupies one `active_async` slot while it is in any non-quiescent state
/// (`RUNNABLE`, `RUNNING`, `RUNNING_NOTIFIED`); it releases the slot only on the
/// transition to `PARKED`, on completion, or on drop.
const PARKED: u8 = 0;
const RUNNABLE: u8 = 1;
const RUNNING: u8 = 2;
const RUNNING_NOTIFIED: u8 = 3;
const DONE: u8 = 4;

/// Per-task gate for quiescence accounting. It tracks whether the task currently
/// occupies an `active_async` slot and INTERCEPTS every wake (it is handed to the
/// inner future as its `Waker`), so a task that has been woken — its waker fired
/// and it is queued to be polled — is counted from that instant until it is next
/// polled. This closes the wake→poll window the old per-poll wrapper left open
/// (a runnable-but-not-yet-repolled task was uncounted, so the clock could jump
/// past it). Because the gate IS the inner future's waker, EVERY wake routes
/// through it: engine wakes, the real-I/O reactor, `JoinHandle`, raw channels.
struct TaskGate {
    state: AtomicU8,
    /// The runtime's waker for this task, refreshed each poll. The gate forwards
    /// to it on wake so the real poll is re-scheduled.
    runtime_waker: Mutex<Option<Waker>>,
}

impl TaskGate {
    /// A fresh gate starts `RUNNABLE`: a constructed/spawned task is queued to be
    /// polled, so it occupies a slot at once (acquired by [`participate`]).
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(RUNNABLE),
            runtime_waker: Mutex::new(None),
        })
    }

    fn store_runtime_waker(&self, w: &Waker) {
        let mut g = self.runtime_waker.lock();
        match g.as_ref() {
            Some(existing) if existing.will_wake(w) => {}
            _ => *g = Some(w.clone()),
        }
    }

    fn forward(&self) {
        let w = self.runtime_waker.lock().clone();
        if let Some(w) = w {
            w.wake();
        }
    }

    /// Poll entry: claim the poll iff the task is `RUNNABLE` (it holds a slot and
    /// was genuinely queued). Returns `false` for any other state — a
    /// duplicate/stale schedule (the runtime can poll more times than there are
    /// wakes when several `forward`s race a `park`). The caller MUST then return
    /// `Pending` without polling the inner future or touching the slot, so the
    /// `active_async` accounting stays balanced. The slot is already held from
    /// spawn or the waking transition, so a successful claim changes no counter.
    fn try_enter_poll(&self) -> bool {
        self.state
            .compare_exchange(RUNNABLE, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Poll returned `Ready`: the task is done — release its slot. The `DONE`
    /// store and the counter decrement happen together under the `SCHED` lock.
    fn complete(&self) {
        sched::gate_complete(&self.state, DONE);
    }

    /// Poll returned `Pending`: `RUNNING`→`PARKED` releases the slot (a quiescent
    /// edge); a wake that landed mid-poll left `RUNNING_NOTIFIED`, so the CAS fails
    /// and the gate stays `RUNNABLE`, keeping the slot for the re-poll that wake
    /// already scheduled. The state transition and the counter move atomically
    /// under the `SCHED` lock so a concurrent wake cannot interleave — see
    /// [`sched::gate_park`].
    fn park(&self) {
        sched::gate_park(&self.state, RUNNING, PARKED, RUNNABLE);
    }

    /// Drop: release the slot iff the task still occupies one (`RUNNABLE`/`RUNNING`/
    /// `RUNNING_NOTIFIED`). `PARKED` and `DONE` hold none.
    fn on_drop(&self) {
        sched::gate_drop_release(&self.state, DONE, RUNNABLE, RUNNING, RUNNING_NOTIFIED);
    }
}

impl Wake for TaskGate {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        loop {
            match self.state.load(Ordering::Acquire) {
                PARKED => {
                    // Re-acquire the slot the park released BEFORE the real poll
                    // runs: this wake→poll window must stay counted so the clock
                    // cannot jump past this task. The CAS and the acquire happen
                    // together under the `SCHED` lock so a concurrent `park`'s
                    // release cannot cancel this acquire — see
                    // [`sched::gate_wake_parked`]. A `false` return means the state
                    // left `PARKED` between the load and the CAS; loop to re-read.
                    if sched::gate_wake_parked(&self.state, PARKED, RUNNABLE) {
                        self.forward();
                        return;
                    }
                }
                RUNNING => {
                    if self
                        .state
                        .compare_exchange(
                            RUNNING,
                            RUNNING_NOTIFIED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        // Woken during its own poll; slot already held. Forward so
                        // the runtime re-polls after the current poll returns.
                        self.forward();
                        return;
                    }
                }
                // RUNNABLE / RUNNING_NOTIFIED: already pending a poll, slot held —
                // idempotent. DONE: nothing to wake.
                RUNNABLE | RUNNING_NOTIFIED => {
                    self.forward();
                    return;
                }
                _ => return,
            }
        }
    }
}

/// Engine-backed `tokio::task::yield_now` under `sim-time`. A cooperative async
/// yield must let the virtual clock advance — in real time, time passes while a
/// task yields and other work (a server throttle) makes progress. This parks the
/// task as a yield-waiter (its `active_async` slot is released by the spawn gate
/// when the future returns Pending), so the clock is free to reach the next
/// event, then re-polls on the next advance. There is deliberately NO
/// resolve-at-once path: re-polling immediately would re-arm a busy-poll loop
/// that pins `active_async` and freezes the clock (the bug a naive `yield_now`
/// causes under quiescence).
pub struct SimYield {
    handle: Option<(u64, Arc<AtomicBool>)>,
    done: bool,
}

impl Future for SimYield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        if let Some((_, granted)) = self.handle.as_ref() {
            if granted.load(Ordering::Acquire) {
                self.done = true;
                self.handle = None;
                return Poll::Ready(());
            }
            return Poll::Pending;
        }
        self.handle = Some(sched::register_yield_async(cx.waker().clone()));
        Poll::Pending
    }
}

impl Drop for SimYield {
    fn drop(&mut self) {
        if let Some((id, _)) = self.handle.take() {
            sched::cancel_yield(id);
        }
    }
}

/// Cooperative async yield that participates in quiescence (see [`SimYield`]).
pub fn yield_now() -> SimYield {
    SimYield {
        handle: None,
        done: false,
    }
}

/// Spawn poll-wrapper: keeps the wrapped task counted in the engine's
/// `active_async` for as long as it is non-quiescent — from becoming runnable
/// (spawned, or woken) until it next parks, completes, or drops — via its
/// [`TaskGate`]. The gate (not the per-poll bracket) is what closes the wake→poll
/// window: a task whose waker has fired but which has not yet been re-polled
/// stays counted, so the virtual clock cannot advance past a runnable task.
/// Installed at the spawn chokepoint ([`crate::tokio::task::spawn`]) and on the
/// test root task, so every async task on the sim path participates.
pub struct Participating<F> {
    fut: F,
    gate: Arc<TaskGate>,
}

impl<F: Future> Future for Participating<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: `fut` is structurally pinned and never moved out; `gate`
        // (`Arc`) is `Unpin` and only touched through shared refs. We re-pin
        // `fut` in place via `Pin::new_unchecked`, so projecting the outer pin
        // with `get_unchecked_mut` is sound.
        let this = unsafe { self.get_unchecked_mut() };
        this.gate.store_runtime_waker(cx.waker());
        if !this.gate.try_enter_poll() {
            // Duplicate/stale schedule: the task is parked (or done), holding no
            // slot. Stay pending without re-polling the inner future — the real
            // wake will re-arm it.
            return Poll::Pending;
        }
        let gate_waker = Waker::from(Arc::clone(&this.gate));
        let mut gate_cx = Context::from_waker(&gate_waker);
        let fut = unsafe { Pin::new_unchecked(&mut this.fut) };
        // Mark this OS thread as inside an async poll for the duration of the
        // inner poll, so a synchronous wrapped wait taken from within it (e.g. a
        // blocking `recv_sync` reaching the engine) is treated as a BRIDGED wait —
        // releasing this task's `active_async` slot while it blocks instead of
        // pinning the clock. Drops (restoring the depth) even if the poll unwinds.
        let outcome = {
            let _poll_guard = sched::AsyncPollGuard::enter();
            fut.poll(&mut gate_cx)
        };
        match outcome {
            Poll::Ready(out) => {
                this.gate.complete();
                Poll::Ready(out)
            }
            Poll::Pending => {
                this.gate.park();
                Poll::Pending
            }
        }
    }
}

impl<F> Drop for Participating<F> {
    fn drop(&mut self) {
        self.gate.on_drop();
    }
}

/// Wrap `fut` so it participates in quiescence accounting (see [`Participating`]).
/// The task occupies an `active_async` slot immediately — a constructed/spawned
/// task is runnable until polled — balanced when it parks, completes, or drops.
pub fn participate<F: Future>(fut: F) -> Participating<F> {
    sched::async_acquire();
    Participating {
        fut,
        gate: TaskGate::new(),
    }
}

/// Process-global virtual timeline, in nanoseconds. Only moves forward, via the
/// `sched` quiescence engine (and the test-only additive [`advance`]); starts
/// at [`Instant::BASE_NANOS`].
static SIM_NANOS: AtomicU64 = AtomicU64::new(Instant::BASE_NANOS);

/// Current virtual instant in nanoseconds since the timeline origin. Read by the
/// engine harness tests; production reads `SIM_NANOS` directly under `SCHED` or
/// via `Instant::as_virtual_nanos`.
#[cfg(test)]
#[inline]
pub(crate) fn now_nanos() -> u64 {
    SIM_NANOS.load(Ordering::Acquire)
}

fn duration_to_nanos(d: Duration) -> u64 {
    // Fold via `u64` seconds + `u32` subsec — no `u128` intermediate, no cast.
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    d.as_secs()
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(u64::from(d.subsec_nanos()))
}

/// Manually advance the virtual clock by `delta`. Additive and test-only: the
/// production clock is driven solely by the quiescence engine (`sched`), so the
/// engine is the single clock writer. The 4 arithmetic clock tests use this as
/// a manual bump to exercise `Instant` arithmetic without the engine.
#[cfg(test)]
#[inline]
pub(crate) fn advance(delta: Duration) {
    SIM_NANOS.fetch_add(duration_to_nanos(delta), Ordering::Release);
}

/// Reset the timeline to its base and clear the quiescence engine. For unit
/// tests that share one process; production tests get per-test process
/// isolation from nextest. Order matters: store the base first, then drop the
/// engine state, so afterwards the clock reads `Instant::BASE_NANOS` and the
/// engine is empty.
#[inline]
pub fn reset() {
    SIM_NANOS.store(Instant::BASE_NANOS, Ordering::Release);
    sched::reset();
}

thread_local! {
    /// Nesting depth of [`RealTimeScope`] on this thread. `0` means the virtual
    /// clock is in effect (the default under `sim-time`); any non-zero value
    /// means this thread reads REAL time and uses REAL timers for the scope's
    /// duration. A depth (not a bool) so nested scopes compose.
    static SIM_OFF: Cell<u32> = const { Cell::new(0) };
}

/// True when the virtual clock governs this thread (the `sim-time` default).
/// False inside a [`RealTimeScope`] — the thread is on real wall-clock time and
/// real timers, and is NOT a quiescence participant for those waits.
///
/// This is the per-thread switch the rest of the platform consults: `Instant::
/// now`, `thread::park_timeout`, and `sync::Condvar` all branch on it so a
/// real-time island (the hang watchdog's clock reads, a real blocking I/O
/// stretch) lives on true wall-clock while the surrounding test still collapses
/// its virtual waits on the engine.
#[inline]
#[must_use]
pub fn sim_enabled() -> bool {
    SIM_OFF.with(|c| c.get() == 0)
}

/// RAII guard that puts the current thread on REAL time for its lifetime: while
/// held, [`Instant::now`] reads the real monotonic clock and the synchronous
/// wait primitives use their real implementations instead of the engine. Drop
/// restores the previous mode (scopes nest).
///
/// Intended for SYNCHRONOUS islands only (it keys off a thread-local, so holding
/// it across an `.await` on a shared executor would leak the mode to other
/// tasks). The hang watchdog uses it so its progress deadline is measured in
/// real time — the engine may collapse virtual waits and jump the virtual clock,
/// but the watchdog stays a real-wall-clock safety net that only fires on a true
/// stall.
#[must_use]
pub struct RealTimeScope {
    _priv: (),
}

/// Enter a [`RealTimeScope`]: read real time / use real timers on this thread
/// until the returned guard drops.
#[must_use]
pub fn real_time() -> RealTimeScope {
    SIM_OFF.with(|c| c.set(c.get().saturating_add(1)));
    RealTimeScope { _priv: () }
}

impl Drop for RealTimeScope {
    fn drop(&mut self) {
        SIM_OFF.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Process anchor for the real monotonic clock, sampled once on first use. Real
/// instants are reported as `BASE_NANOS + elapsed-since-anchor`, so a thread in
/// a [`RealTimeScope`] sees a forward-moving clock in the same nanos space as
/// the virtual one (the two are never compared across the boundary — a watchdog
/// samples both its start and its checks in the same mode).
fn real_now_nanos() -> u64 {
    static REAL_EPOCH: OnceLock<web_time::Instant> = OnceLock::new();
    let epoch = REAL_EPOCH.get_or_init(web_time::Instant::now);
    let elapsed = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Instant::BASE_NANOS.saturating_add(elapsed)
}

/// Drop-in for `web_time::Instant` backed by the virtual clock. Exposes exactly
/// the API surface the workspace uses on instants (`now`, `elapsed`,
/// `duration_since`, `saturating_duration_since`, `+`/`-`, ordering); all
/// arithmetic saturates so misuse never panics or wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Timeline origin: one day in, so realistic backward offsets from `now()`
    /// (e.g. crossfade start instants) stay positive; arithmetic saturates anyway.
    const BASE_NANOS: u64 = 86_400_000_000_000;

    #[inline]
    #[must_use]
    pub fn now() -> Self {
        if sim_enabled() {
            Self(SIM_NANOS.load(Ordering::Acquire))
        } else {
            Self(real_now_nanos())
        }
    }

    /// Absolute virtual nanoseconds this instant represents. Used by the
    /// platform `Condvar` to convert a deadline into the engine's nanos space.
    #[inline]
    pub(crate) fn as_virtual_nanos(self) -> u64 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    #[inline]
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.saturating_duration_since(earlier)
    }

    #[inline]
    #[must_use]
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(duration_to_nanos(rhs)))
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Duration) -> Self {
        Self(self.0.saturating_sub(duration_to_nanos(rhs)))
    }
}

impl Sub<Self> for Instant {
    type Output = Duration;
    #[inline]
    fn sub(self, rhs: Self) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

#[cfg(test)]
mod tests;
