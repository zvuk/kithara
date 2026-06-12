use std::{
    future::Future,
    marker::PhantomData,
    ops::{Add, Sub},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use pin_project_lite::pin_project;

pub use super::participant::{Participating, participate};
use super::{
    ctx::{self, ModeSnapshot, flash_ambient, flash_enabled},
    ids::WaiterId,
    system::{self, FLASH},
};
pub use crate::common::time::Duration;
use crate::flash::time::{FlashTimeout, TimeoutError};

/// RAII bracket for ONE real I/O operation in flight (a socket send / response
/// or body-chunk await in `kithara-net`). While at least one scope is live the
/// virtual clock is PACED: it may not advance beyond the real time elapsed
/// since the first scope opened, so a virtual watchdog or timeout racing the
/// real-world transit fires only after the equivalent REAL time — never
/// spuriously ahead of bytes still on the wire. Pace, not pin: a deliberate
/// virtual delay behind the op (a virtually-delayed test server) still
/// elapses at real pace, so the peer stays live. Dropping the last scope
/// resumes full-speed collapse.
#[must_use]
pub struct RealIoScope {
    _priv: (),
}

/// Open a [`RealIoScope`] (see its contract).
pub fn real_io() -> RealIoScope {
    system::real_io_enter();
    RealIoScope { _priv: () }
}

impl Drop for RealIoScope {
    fn drop(&mut self) {
        system::real_io_exit();
    }
}

/// Engine-backed `sleep` future: registers a virtual deadline + the task waker
/// on the quiescence engine on its first poll, then resolves once the engine
/// crosses that deadline. Collapses to zero wall-clock (the clock jumps when all
/// participants park). Resolution is GRANT-driven (`handle.granted()`), never a
/// bare clock check; the task's `active_async` slot is owned by the spawn
/// poll-wrapper gate ([`Participating`]), so this future touches no counter.
pub(crate) struct FlashSleep {
    delta_nanos: u64,
    handle: Option<system::AsyncHandle>,
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
        let (handle, adv) = system::register_sleep_async(self.delta_nanos, cx.waker().clone());
        self.handle = Some(handle);
        adv.fire();
        Poll::Pending
    }
}

impl Drop for FlashSleep {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            system::cancel_async_wait(&handle);
        }
    }
}

/// Engine-backed `tokio::task::yield_now` under `flash`. A cooperative async
/// yield must let the virtual clock advance — in real time, time passes while a
/// task yields and other work (a server throttle) makes progress. This parks the
/// task as a yield-waiter (its `active_async` slot is released by the spawn gate
/// when the future returns Pending), so the clock is free to reach the next
/// event, then re-polls on the next advance. There is deliberately NO
/// resolve-at-once path: re-polling immediately would re-arm a busy-poll loop
/// that pins `active_async` and freezes the clock (the bug a naive `yield_now`
/// causes under quiescence).
pub struct FlashYield {
    handle: Option<(WaiterId, Arc<AtomicBool>)>,
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
        let (id, granted, adv) = system::register_yield_async(cx.waker().clone());
        self.handle = Some((id, granted));
        adv.fire();
        Poll::Pending
    }
}

impl Drop for FlashYield {
    fn drop(&mut self) {
        if let Some((id, _)) = self.handle.take() {
            system::cancel_yield(id);
        }
    }
}

/// Cooperative async yield. Like the stateful sync primitives (Condvar/Notify/
/// mpsc/oneshot), this branches on [`flash_ambient`], NOT [`flash_enabled`]:
/// engine-backed ([`FlashYield`]) only inside a flash-eligible (ambient) test,
/// and a real `tokio::task::yield_now` otherwise. A `yield_now`'s resolution
/// comes from an engine clock advance, whose grant requires `active_async == 0`;
/// in a flash(false) test the surrounding task's other primitives are REAL, so it
/// keeps its `active_async` slot across the yield, and an engine-backed yield can
/// never be granted (a circular dependency — `active_async` never hits zero while
/// the only `.await` blocking the task is the yield). Gating on ambient keeps the
/// flash BUILD behavior-transparent for ambient=false (flash(false) tests AND
/// production), exactly as the stateful-primitive ambient gate does.
pub fn yield_now() -> Yield {
    if flash_ambient() {
        Yield::Flash(FlashYield {
            handle: None,
            done: false,
        })
    } else {
        Yield::Real { yielded: false }
    }
}

/// Ambient-gated cooperative yield future (see [`yield_now`]). Engine-backed under
/// ambient, a plain scheduler yield otherwise. The mode is fixed at construction
/// from the ambient gate, which is uniform per test.
#[must_use = "a Yield future does nothing unless `.await`ed"]
pub enum Yield {
    /// Engine-backed quiescence yield (ambient test).
    Flash(FlashYield),
    /// Real cooperative yield (ambient off: flash(false) test / production):
    /// returns `Pending` once after re-arming the waker, then `Ready` — the same
    /// hand-back-to-the-scheduler semantics as `tokio::task::yield_now`, but
    /// without naming `tokio`'s unnameable yield future.
    Real { yielded: bool },
}

impl Future for Yield {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // `Yield` is `Unpin` (every field of both variants is), so the safe
        // `get_mut`/`Pin::new` projection is available — and the compiler
        // enforces the premise: this stops building if a variant ever gains a
        // `!Unpin` field. Same shape as the inert mirror in
        // `common/flash_inert.rs`.
        match self.get_mut() {
            Self::Flash(f) => Pin::new(f).poll(cx),
            Self::Real { yielded } => {
                if *yielded {
                    Poll::Ready(())
                } else {
                    *yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }
    }
}

/// Dump the flash quiescence-engine state to stderr. The `#[kithara::test]`
/// harness calls this from both hang exits (virtual-timeout panic and the
/// HARD TIMEOUT abort thread) so a wedged run self-reports every parked
/// participant, deadline and pending signal instead of dying opaque.
pub fn dump_to_stderr(context: &str) {
    eprintln!("[flash-dump] {context}:\n{}", system::dump());
}

/// Virtual `sleep` that hits the quiescence engine UNCONDITIONALLY (no
/// `flash_enabled()` consult). The lexical test rewriter ([`#[kithara::test(flash(true))]`])
/// retargets a test body's direct `time::sleep` calls here, so the BODY's own
/// waits collapse onto virtual time without setting the active mode flag — a
/// prod fn the body calls keeps its stateless time reads on REAL.
pub fn virtual_sleep(duration: Duration) -> impl Future<Output = ()> {
    FlashSleep::new(duration)
}

/// Virtual `timeout` that hits the engine UNCONDITIONALLY (see
/// [`virtual_sleep`]). Races `future` against an engine-backed deadline.
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
pub async fn virtual_timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    FlashTimeout {
        future,
        sleep: FlashSleep::new(duration),
    }
    .await
}

/// Virtual `Instant::now` read UNCONDITIONALLY from the engine clock (see
/// [`virtual_sleep`]). Mirrors [`Instant::now`]'s flash arm.
#[must_use]
pub fn virtual_now() -> Instant {
    Instant::now_virtual()
}

/// Virtual `park_timeout` that hits the engine UNCONDITIONALLY (see
/// [`virtual_sleep`]). Mirrors [`crate::thread::park_timeout`]'s flash arm.
pub fn virtual_park_timeout(duration: Duration) {
    crate::thread::park_timeout_virtual(duration);
}

pub(super) fn duration_to_nanos(d: Duration) -> u64 {
    // Fold via `u64` seconds + `u32` subsec — no `u128` intermediate, no cast.
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    d.as_secs()
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(u64::from(d.subsec_nanos()))
}

/// Manually advance the virtual clock by `delta`. Additive and test-only: the
/// production clock is driven solely by the quiescence engine, so the engine
/// is the single clock writer. The 4 arithmetic clock tests use this as a
/// manual bump to exercise `Instant` arithmetic without the engine.
#[cfg(test)]
#[inline]
pub(crate) fn advance(delta: Duration) {
    FLASH.clock.advance(duration_to_nanos(delta));
}

/// Reset the timeline to its base and clear the quiescence engine. For unit
/// tests that share one process; production tests get per-test process
/// isolation from nextest. See `FlashInner::reset` for the ordering contract.
#[inline]
pub fn reset() {
    FLASH.reset();
}

/// RAII guard for a prod `#[kithara::flash(bool)]` region. `on=true` activates
/// flash for the dynamic extent IFF the test is flash-eligible (ambient);
/// `on=false` carves REAL inside a flash region. Saves/restores the previous
/// whole [`Mode`] so regions nest bidirectionally (LIFO premise — see
/// `flash/ctx.rs`). `!Send`: it restores THIS thread's mode, so moving it to
/// another thread would restore the wrong thread's state.
#[must_use]
pub struct FlashScope(ModeSnapshot, PhantomData<*mut ()>);

impl Drop for FlashScope {
    fn drop(&mut self) {
        ctx::restore_mode(self.0);
    }
}

/// Push a dynamic flash mode. `on=true` takes only under ambient; `on=false`
/// always carves real. Returns a guard that restores the previous mode on drop.
pub fn enter_dynamic(on: bool) -> FlashScope {
    FlashScope(ctx::push_active(on), PhantomData)
}

/// Enter a REAL-time carve on this thread (flash off for the guard's lifetime).
/// In the default-real model this only matters inside an active flash region;
/// kept for the real-socket test-server island and the off-feature stub.
pub fn flash_real() -> FlashScope {
    enter_dynamic(false)
}

/// RAII guard setting the per-test ambient gate (test macro + spawn
/// propagation). Saves/restores the previous whole [`Mode`] on drop (LIFO
/// premise — see `flash/ctx.rs`). `!Send`: it restores THIS thread's mode.
/// Sanctioned exception to "never hold a scope across `.await`": the test
/// macro's WASM body may hold one (single-threaded driver, sole ambient
/// writer there). Async-native emissions hold NONE: a body-held scope inside
/// the cancellable timeout would tear down non-LIFO on `Elapsed`.
#[must_use]
pub struct AmbientScope(ModeSnapshot, PhantomData<*mut ()>);

impl Drop for AmbientScope {
    fn drop(&mut self) {
        ctx::restore_mode(self.0);
    }
}

/// Set the per-test ambient gate; restores the previous mode on drop. The test
/// macro sets it for the test body; the platform spawn wrappers re-establish it
/// on each spawned child via [`set_ambient_for_spawn`].
pub fn ambient_scope(on: bool) -> AmbientScope {
    AmbientScope(ctx::push_ambient(on), PhantomData)
}

/// Snapshot the per-test ambient gate (for spawn propagation into a child).
/// Reads the same gate as [`flash_ambient`]; kept as the named spawn-capture
/// entry point for B5's propagation call sites.
#[inline]
#[must_use]
pub fn ambient_snapshot() -> bool {
    flash_ambient()
}

/// Restore a snapshotted ambient on a spawned child, held for its lifetime.
pub fn set_ambient_for_spawn(on: bool) -> AmbientScope {
    ambient_scope(on)
}

pin_project! {
    /// Per-poll ambient assertion for a spawned async task. A tokio task can be
    /// polled on different worker threads across its lifetime, so a one-time ambient
    /// set on the spawning thread would not stick; this re-asserts the snapshotted
    /// ambient for the duration of each poll (the guard drops when the poll returns,
    /// restoring the worker thread's previous ambient). Installed at the async spawn
    /// chokepoint composed around [`participate`].
    pub struct WithAmbient<F> {
        on: bool,
        #[pin]
        fut: F,
    }
}

impl<F: Future> Future for WithAmbient<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        // The guard is a named binding, so it drops AFTER `fut.poll(cx)`
        // returns, restoring the worker thread's previous ambient.
        let _a = set_ambient_for_spawn(*this.on);
        this.fut.poll(cx)
    }
}

/// Wrap `fut` so the snapshotted ambient is re-asserted around every poll (see
/// [`WithAmbient`]).
pub fn with_ambient<F: Future>(on: bool, fut: F) -> WithAmbient<F> {
    WithAmbient { on, fut }
}

pin_project! {
    /// Per-poll dynamic-flash assertion for an async PROD `#[kithara::flash(bool)]`
    /// region. The async analogue of the sync [`enter_dynamic`] RAII guard: an async
    /// fn can be polled across `.await` on different worker threads, so a one-time
    /// `enter_dynamic` on the first poll would not survive a yield. This re-asserts
    /// the mode for the duration of EACH poll (the guard drops when the poll returns,
    /// restoring the thread's previous mode — no leak across tasks). Same shape as
    /// [`WithAmbient`], with `enter_dynamic` in place of the ambient set.
    pub struct FlashDynamic<F> {
        on: bool,
        #[pin]
        fut: F,
    }
}

impl<F: Future> Future for FlashDynamic<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        // The `_g` guard is a named binding, so it drops AFTER `fut.poll(cx)`
        // returns, restoring the previous mode.
        let _g = enter_dynamic(*this.on);
        this.fut.poll(cx)
    }
}

/// Wrap `fut` so the dynamic flash mode is re-asserted around every poll (see
/// [`FlashDynamic`]).
pub fn dynamic<F: Future>(on: bool, fut: F) -> FlashDynamic<F> {
    FlashDynamic { on, fut }
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
    /// Real instants are reported in the same nanos space (`BASE_NANOS + elapsed`
    /// since the engine clock's real anchor, see `Clock::real_now_nanos`), so a
    /// thread in a [`FlashScope`] sees a forward-moving clock either way (the two
    /// arms are never compared across the boundary — a watchdog samples both its
    /// start and its checks in the same mode).
    pub(in crate::flash) const BASE_NANOS: u64 = 86_400_000_000_000;

    #[inline]
    #[must_use]
    pub fn now() -> Self {
        if flash_enabled() {
            Self::now_virtual()
        } else {
            Self(FLASH.clock.real_now_nanos())
        }
    }

    /// The virtual `now`, read UNCONDITIONALLY from the engine clock (no
    /// `flash_enabled()` consult). The lexical test rewriter (`virtual_now`)
    /// targets this directly so a flash test body's `Instant::now` collapses
    /// onto virtual time without setting the active mode flag.
    #[inline]
    #[must_use]
    pub fn now_virtual() -> Self {
        Self(FLASH.clock.now_nanos())
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
