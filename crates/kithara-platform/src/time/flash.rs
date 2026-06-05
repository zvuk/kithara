pub use std::time::Duration;
use std::{
    cell::Cell,
    future::Future,
    ops::{Add, Sub},
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

/// Quiescence-driven virtual-clock engine. Submodule of `flash`, which is already
/// gated on `feature = "flash-time"` + native, so it needs no extra feature gate.
/// The engine drives `SIM_NANOS` forward at quiescent points. Its consumers are
/// the platform wait primitives (`thread::park_timeout`, `sync::Condvar`,
/// async `FlashSleep`/`Notify`) plus the harness, so it compiles whenever
/// `flash-time` is on. The engine API stays `pub(crate)`.
pub mod sched;

mod participant;
mod wake;

pub use participant::{Participating, participate};

/// Engine-backed `sleep` future: registers a virtual deadline + the task waker
/// on the quiescence engine on its first poll, then resolves once the engine
/// crosses that deadline. Collapses to zero wall-clock (the clock jumps when all
/// participants park). Resolution is GRANT-driven (`handle.granted()`), never a
/// bare clock check; the task's `active_async` slot is owned by the spawn
/// poll-wrapper gate ([`Participating`]), so this future touches no counter.
pub struct FlashSleep {
    delta_nanos: u64,
    handle: Option<sched::AsyncHandle>,
}

impl FlashSleep {
    pub(crate) fn new(duration: Duration) -> Self {
        Self {
            delta_nanos: duration_to_nanos(duration),
            handle: None,
        }
    }
}

impl Future for FlashSleep {
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

impl Drop for FlashSleep {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            sched::cancel_async_wait(handle);
        }
    }
}

/// Engine-backed `tokio::task::yield_now` under `flash-time`. A cooperative async
/// yield must let the virtual clock advance — in real time, time passes while a
/// task yields and other work (a server throttle) makes progress. This parks the
/// task as a yield-waiter (its `active_async` slot is released by the spawn gate
/// when the future returns Pending), so the clock is free to reach the next
/// event, then re-polls on the next advance. There is deliberately NO
/// resolve-at-once path: re-polling immediately would re-arm a busy-poll loop
/// that pins `active_async` and freezes the clock (the bug a naive `yield_now`
/// causes under quiescence).
pub struct FlashYield {
    handle: Option<(u64, Arc<AtomicBool>)>,
    done: bool,
}

impl Future for FlashYield {
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

impl Drop for FlashYield {
    fn drop(&mut self) {
        if let Some((id, _)) = self.handle.take() {
            sched::cancel_yield(id);
        }
    }
}

/// Cooperative async yield that participates in quiescence (see [`FlashYield`]).
pub fn yield_now() -> FlashYield {
    FlashYield {
        handle: None,
        done: false,
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
    /// Nesting depth of [`FlashScope`] on this thread. `0` means the virtual
    /// clock is in effect (the default under `flash-time`); any non-zero value
    /// means this thread reads REAL time and uses REAL timers for the scope's
    /// duration. A depth (not a bool) so nested scopes compose.
    static FLASH_OFF: Cell<u32> = const { Cell::new(0) };
}

/// True when the virtual clock governs this thread (the `flash-time` default).
/// False inside a [`FlashScope`] — the thread is on real wall-clock time and
/// real timers, and is NOT a quiescence participant for those waits.
///
/// This is the per-thread switch the rest of the platform consults: `Instant::
/// now`, `thread::park_timeout`, and `sync::Condvar` all branch on it so a
/// real-time island (the hang watchdog's clock reads, a real blocking I/O
/// stretch) lives on true wall-clock while the surrounding test still collapses
/// its virtual waits on the engine.
#[inline]
#[must_use]
pub fn flash_enabled() -> bool {
    FLASH_OFF.with(|c| c.get() == 0)
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
pub struct FlashScope {
    _priv: (),
}

/// Enter a [`FlashScope`]: read real time / use real timers on this thread
/// until the returned guard drops.
#[must_use]
pub fn flash_real() -> FlashScope {
    FLASH_OFF.with(|c| c.set(c.get().saturating_add(1)));
    FlashScope { _priv: () }
}

impl Drop for FlashScope {
    fn drop(&mut self) {
        FLASH_OFF.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Process anchor for the real monotonic clock, sampled once on first use. Real
/// instants are reported as `BASE_NANOS + elapsed-since-anchor`, so a thread in
/// a [`FlashScope`] sees a forward-moving clock in the same nanos space as
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
        if flash_enabled() {
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
