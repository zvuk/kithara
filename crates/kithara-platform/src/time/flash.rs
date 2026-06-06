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
        let (id, granted, adv) = sched::register_yield_async(cx.waker().clone());
        self.handle = Some((id, granted));
        sched::fire_advance(adv);
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

/// Cooperative async yield. Like the stateful sync primitives (Condvar/Notify/
/// mpsc/oneshot), this branches on [`flash_ambient`], NOT [`flash_enabled`]:
/// engine-backed ([`FlashYield`]) only inside a flash-eligible (ambient) test,
/// and a real `tokio::task::yield_now` otherwise. A `yield_now`'s resolution
/// comes from an engine clock advance, whose grant requires `active_async == 0`;
/// in a flash(false) test the surrounding task's other primitives are REAL, so it
/// keeps its `active_async` slot across the yield, and an engine-backed yield can
/// never be granted (a circular dependency — `active_async` never hits zero while
/// the only `.await` blocking the task is the yield). Gating on ambient keeps the
/// flash-time BUILD behavior-transparent for ambient=false (flash(false) tests AND
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
        // SAFETY: the `Flash` inner future is structurally pinned and re-pinned in
        // place via `Pin::new_unchecked`; the `Real` variant holds only a `Copy`
        // scalar touched by value.
        let this = unsafe { self.get_unchecked_mut() };
        match this {
            Self::Flash(f) => unsafe { Pin::new_unchecked(f) }.poll(cx),
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
    /// Per-test gate: "is this test flash-eligible?" Set by the test macro,
    /// propagated across spawn. Default false = not a flash test. A gate —
    /// consumers never read it directly; only [`enter_dynamic`] consults it to
    /// decide whether a prod flash region may take effect.
    static FLASH_AMBIENT: Cell<bool> = const { Cell::new(false) };
    /// Dynamic: "is flash propagating on this callstack right now?" Pushed by a
    /// prod `#[kithara::flash(true)]` guard (only when ambient). Read by the time
    /// primitives via [`flash_enabled`]. Default false = REAL.
    static FLASH_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// True when flash (virtual clock) governs this callstack. Default false (REAL).
/// The per-thread switch the STATELESS time primitives consult: `Instant::now`,
/// `thread::park_timeout`, `thread::sleep`/`yield_now`/`unpark`, `sleep`/`timeout`
/// branch on it.
#[inline]
#[must_use]
pub fn flash_enabled() -> bool {
    FLASH_ACTIVE.with(Cell::get)
}

/// True when the current test is flash-eligible (the per-test ambient gate,
/// propagated across spawn). The STATEFUL sync primitives (Condvar/Notify/mpsc/
/// oneshot) branch on THIS — not `flash_enabled()` — so a primitive's wait and
/// its cross-thread signal always agree on real-vs-engine (ambient is uniform
/// per test; FLASH_ACTIVE is per-callstack and would mismatch across threads).
#[inline]
#[must_use]
pub fn flash_ambient() -> bool {
    FLASH_AMBIENT.with(Cell::get)
}

/// RAII guard for a prod `#[kithara::flash(bool)]` region. `on=true` activates
/// flash for the dynamic extent IFF the test is flash-eligible (ambient);
/// `on=false` carves REAL inside a flash region. Saves/restores the previous
/// `FLASH_ACTIVE` so regions nest bidirectionally.
#[must_use]
pub struct FlashScope(bool);

impl Drop for FlashScope {
    fn drop(&mut self) {
        FLASH_ACTIVE.with(|c| c.set(self.0));
    }
}

/// Push a dynamic flash mode. `on=true` takes only under ambient; `on=false`
/// always carves real. Returns a guard that restores the previous mode on drop.
#[must_use]
pub fn enter_dynamic(on: bool) -> FlashScope {
    let prev = FLASH_ACTIVE.with(Cell::get);
    let next = on && FLASH_AMBIENT.with(Cell::get);
    FLASH_ACTIVE.with(|c| c.set(next));
    FlashScope(prev)
}

/// Enter a REAL-time carve on this thread (flash off for the guard's lifetime).
/// In the default-real model this only matters inside an active flash region;
/// kept for the real-socket test-server island and the off-feature stub.
#[must_use]
pub fn flash_real() -> FlashScope {
    enter_dynamic(false)
}

/// RAII guard setting the per-test ambient gate (test macro + spawn
/// propagation). Saves/restores the previous `FLASH_AMBIENT` on drop.
#[must_use]
pub struct AmbientScope(bool);

impl Drop for AmbientScope {
    fn drop(&mut self) {
        FLASH_AMBIENT.with(|c| c.set(self.0));
    }
}

/// Set the per-test ambient gate; restores the previous value on drop. The test
/// macro sets it for the test body; the platform spawn wrappers re-establish it
/// on each spawned child via [`set_ambient_for_spawn`].
#[must_use]
pub fn ambient_scope(on: bool) -> AmbientScope {
    let prev = FLASH_AMBIENT.with(Cell::get);
    FLASH_AMBIENT.with(|c| c.set(on));
    AmbientScope(prev)
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
#[must_use]
pub fn set_ambient_for_spawn(on: bool) -> AmbientScope {
    ambient_scope(on)
}

/// Per-poll ambient assertion for a spawned async task. A tokio task can be
/// polled on different worker threads across its lifetime, so a one-time ambient
/// set on the spawning thread would not stick; this re-asserts the snapshotted
/// ambient for the duration of each poll (the guard drops when the poll returns,
/// restoring the worker thread's previous ambient). Installed at the async spawn
/// chokepoint composed around [`participate`].
pub struct WithAmbient<F> {
    on: bool,
    fut: F,
}

impl<F: Future> Future for WithAmbient<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: `fut` is structurally pinned and never moved out of
        // `WithAmbient`; it is re-pinned in place for its own poll. `on` is a
        // `Copy` scalar touched only by value.
        let this = unsafe { self.get_unchecked_mut() };
        let _a = set_ambient_for_spawn(this.on);
        let fut = unsafe { Pin::new_unchecked(&mut this.fut) };
        fut.poll(cx)
    }
}

/// Wrap `fut` so the snapshotted ambient is re-asserted around every poll (see
/// [`WithAmbient`]).
pub fn with_ambient<F: Future>(on: bool, fut: F) -> WithAmbient<F> {
    WithAmbient { on, fut }
}

/// Per-poll dynamic-flash assertion for an async PROD `#[kithara::flash(bool)]`
/// region. The async analogue of the sync [`enter_dynamic`] RAII guard: an async
/// fn can be polled across `.await` on different worker threads, so a one-time
/// `enter_dynamic` on the first poll would not survive a yield. This re-asserts
/// the mode for the duration of EACH poll (the guard drops when the poll returns,
/// restoring the thread's previous `FLASH_ACTIVE` — no leak across tasks). Same
/// shape as [`WithAmbient`], with `enter_dynamic` in place of the ambient set.
pub struct FlashDynamic<F> {
    on: bool,
    fut: F,
}

impl<F: Future> Future for FlashDynamic<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: `fut` is structurally pinned and never moved out of
        // `FlashDynamic`; it is re-pinned in place for its own poll. `on` is a
        // `Copy` scalar touched only by value. The `_g` guard is a named binding,
        // so it drops AFTER `fut.poll(cx)` returns, restoring the previous
        // `FLASH_ACTIVE`.
        let this = unsafe { self.get_unchecked_mut() };
        let _g = enter_dynamic(this.on);
        let fut = unsafe { Pin::new_unchecked(&mut this.fut) };
        fut.poll(cx)
    }
}

/// Wrap `fut` so the dynamic flash mode is re-asserted around every poll (see
/// [`FlashDynamic`]).
pub fn flash_dynamic<F: Future>(on: bool, fut: F) -> FlashDynamic<F> {
    FlashDynamic { on, fut }
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
            Self::now_virtual()
        } else {
            Self(real_now_nanos())
        }
    }

    /// The virtual `now`, read UNCONDITIONALLY from the engine clock (no
    /// `flash_enabled()` consult). The lexical test rewriter (`flash_virtual_now`)
    /// targets this directly so a flash test body's `Instant::now` collapses
    /// onto virtual time without setting `FLASH_ACTIVE`.
    #[inline]
    #[must_use]
    pub fn now_virtual() -> Self {
        Self(SIM_NANOS.load(Ordering::Acquire))
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
