use std::{
    future::Future,
    marker::PhantomData,
    ops::{Add, AddAssign, Sub},
    pin::Pin,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;

pub use super::participant::{Participating, participate, participate_sync};
use super::{
    ctx::{self, ModeSnapshot, flash_ambient, flash_enabled},
    system::{self, FLASH},
};
pub use crate::common::time::Duration;

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
/// bare clock check. Its unique handle retains the grant credit until consumed
/// or dropped; the task's separate async slot belongs to its poll-wrapper gate.
pub(crate) struct FlashSleep {
    handle: Option<system::AsyncHandle>,
    delta_nanos: u64,
}

impl FlashSleep {
    pub(crate) fn new(duration: Duration) -> Self {
        Self {
            delta_nanos: duration_to_nanos(duration),
            handle: None,
        }
    }

    /// Register the deadline (and run the advance rule) WITHOUT consuming a
    /// grant. The engine computes a deadline from the clock it reads at
    /// registration, so a caller whose deadline must bound work that can itself
    /// move the clock has to arm before running that work - see
    /// [`crate::flash::time::FlashTimeout`].
    /// Idempotent: arming an already-armed sleep is a no-op.
    pub(crate) fn arm(mut self: Pin<&mut Self>, cx: &mut Context<'_>) {
        if self.handle.is_some() {
            return;
        }
        let (handle, adv) = system::register_sleep_async(self.delta_nanos, cx.waker().clone());
        self.handle = Some(handle);
        adv.fire();
    }
}

impl Future for FlashSleep {
    type Output = ();

    /// Resolves only when the engine grants this waiter, never from a bare clock read past the
    /// deadline, so another advance that jumps the clock past this deadline cannot resolve it
    /// early.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(handle) = self.handle.as_ref() {
            if handle.granted() {
                self.handle = None;
                return Poll::Ready(());
            }
            return Poll::Pending;
        }
        self.arm(cx);
        Poll::Pending
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
    handle: Option<system::AsyncHandle>,
    done: bool,
}

impl Future for FlashYield {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        if let Some(handle) = self.handle.as_ref() {
            if handle.granted() {
                self.done = true;
                self.handle = None;
                return Poll::Ready(());
            }
            return Poll::Pending;
        }
        let (handle, adv) = system::register_yield_async(cx.waker().clone());
        self.handle = Some(handle);
        adv.fire();
        Poll::Pending
    }
}

/// Cooperatively yields through [`FlashYield`] only when [`flash_ambient`] is
/// true, otherwise through real `tokio::task::yield_now`. Unlike stateful
/// primitives, yield has no signal partner and can consult ambient per call.
/// Engine resolution requires `active_async == 0`; a flash(false) task retains
/// its active slot through real primitives, so an engine yield there would
/// wait forever on its own credit. The ambient gate keeps flash(false) tests
/// and production behaviour-transparent.
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

/// Cooperative yield future with its mode fixed at construction.
/// [`yield_now`] selects engine quiescence under ambient eligibility and a scheduler
/// yield otherwise; [`super::tokio::task::yield_runnable`] selects the scheduler mode.
#[must_use = "a Yield future does nothing unless `.await`ed"]
pub enum Yield {
    /// Engine-backed quiescence yield (ambient test).
    Flash(FlashYield),
    /// Scheduler yield, selected by ambient-off `yield_now` or an explicit runnable yield.
    /// Re-arms the waker and returns `Pending` once, then `Ready`; the immediate wake
    /// keeps a participating task runnable across the pending poll.
    Real { yielded: bool },
}

impl Future for Yield {
    type Output = ();

    /// Both `Yield` variants are `Unpin`, so the compiler rejects this code if a variant gains a
    /// `!Unpin` field, keeping the safe `Pin::new` projection sound.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
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

/// Build the full hang report: the dumping thread's context, the quiescence
/// engine snapshot (every parked participant, deadline and pending signal), and
/// the sync-primitive registry snapshot (every live Mutex/RwLock with its
/// holder/waiters + the engine-backed kinds). Pure (no I/O), so a caller can
/// route it to `tracing`, a durable file dump, or both.
#[must_use]
pub fn hang_dump(context: &str) -> String {
    format!(
        "[flash hang dump] {context}\n{thread}\n--- quiescence engine ---\n{engine}\
         --- sync primitives ---\n{registry}",
        thread = super::diag::current_thread_context(),
        engine = system::dump(),
        registry = super::diag::snapshot(),
    )
}

/// Emit [`hang_dump`] via `tracing` at ERROR. The `#[kithara::test]` harness
/// also uses [`hang_dump`] as the rendering owner for durable timeout evidence,
/// so a wedged run can retain the same wait-for picture outside captured logs.
pub fn log_hang_dump(context: &str) {
    tracing::error!(target: "flash::hang", "{}", hang_dump(context));
}

/// Folds seconds and subsec nanoseconds via `u64` arithmetic rather than a `u128` intermediate,
/// avoiding a cast.
pub(super) fn duration_to_nanos(d: Duration) -> u64 {
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

/// Reset a drained unit-test/loom run. No old waiter, grant receipt or
/// participant may remain reachable; nextest isolates product tests.
#[cfg(any(test, feature = "loom"))]
#[inline]
pub(crate) fn reset() {
    FLASH.reset();
}

pub enum DynamicMode {}

pub enum AmbientMode {}

/// RAII guard for a `#[kithara::flash(bool)]` or test-body region. `on=true` activates
/// flash for the dynamic extent IFF the test is flash-eligible (ambient);
/// `on=false` carves REAL inside a flash region. Saves/restores the previous
/// whole `Mode` so regions nest bidirectionally (LIFO premise — see
/// `flash/ctx.rs`). `!Send`: it restores THIS thread's mode, so moving it to
/// another thread would restore the wrong thread's state.
pub type FlashScope = ModeScope<DynamicMode>;

#[must_use]
pub struct ModeScope<Tag>(ModeSnapshot, PhantomData<(*mut (), fn() -> Tag)>);

impl<Tag> Drop for ModeScope<Tag> {
    fn drop(&mut self) {
        ctx::restore_mode(self.0);
    }
}

/// Push a dynamic flash mode. `on=true` takes only under ambient; `on=false`
/// always carves real. Returns a guard that restores the previous mode on drop.
///
/// MACRO-INTERNAL: this is the private expansion target of the
/// `#[kithara::flash(bool)]` and `#[kithara::test]` macros. Do NOT call it by hand —
/// annotate the function with `#[kithara::flash(true|false)]` instead. Direct
/// use is rejected by `just lint`. It stays `pub` only because the macro
/// expands `::kithara_platform::flash::enter_dynamic` into the annotated crate.
#[doc(hidden)]
pub fn enter_dynamic(on: bool) -> FlashScope {
    ModeScope(ctx::push_active(on), PhantomData)
}

/// Enter a REAL-time carve on this thread (flash off for the guard's lifetime).
/// In the default-real model this only matters inside an active flash region;
/// kept for the real-socket test-server island and the off-feature stub.
pub fn flash_real() -> FlashScope {
    enter_dynamic(false)
}

/// RAII guard setting the per-test ambient gate (test macro + spawn
/// propagation). Saves/restores the previous whole `Mode` on drop (LIFO
/// premise — see `flash/ctx.rs`). `!Send`: it restores THIS thread's mode.
/// Sanctioned exception to "never hold a scope across `.await`": the test
/// macro's WASM body may hold one (single-threaded driver, sole ambient
/// writer there). Async-native emissions hold NONE: a body-held scope inside
/// the cancellable timeout would tear down non-LIFO on `Elapsed`.
pub type AmbientScope = ModeScope<AmbientMode>;

/// Set the per-test ambient gate; restores the previous mode on drop. The test
/// macro sets it for the test body; the platform spawn wrappers re-establish it
/// on each spawned child via [`set_ambient_for_spawn`].
pub fn ambient_scope(on: bool) -> AmbientScope {
    ModeScope(ctx::push_ambient(on), PhantomData)
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

    /// The ambient guard is a named binding so it drops after `fut.poll` returns, restoring the
    /// worker thread's previous ambient value across the poll.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
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

    /// The mode guard is a named binding so it drops after `fut.poll` returns, restoring the
    /// previous flash mode across the poll.
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.project();
        let _g = enter_dynamic(*this.on);
        this.fut.poll(cx)
    }
}

/// Wrap `fut` so the dynamic flash mode is re-asserted around every poll (see
/// [`FlashDynamic`]).
///
/// MACRO-INTERNAL: this is the private expansion target of the
/// `#[kithara::flash(bool)]` guard macro (async arm). Do NOT call it by hand —
/// annotate the async function with `#[kithara::flash(true|false)]` instead.
/// Direct use is rejected by `just lint`. It stays `pub` only because the
/// macro expands `::kithara_platform::flash::dynamic` into the annotated crate.
#[doc(hidden)]
pub fn dynamic<F: Future>(on: bool, fut: F) -> FlashDynamic<F> {
    FlashDynamic { on, fut }
}

/// Drop-in for `web_time::Instant` backed by the virtual clock. Exposes exactly
/// the API surface the workspace uses on instants (`now`, `elapsed`,
/// `duration_since`, `saturating_duration_since`, `+`/`+=`/`-`, ordering); all
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

    /// Absolute virtual nanoseconds this instant represents. Used by the
    /// platform `Condvar` to convert a deadline into the engine's nanos space.
    #[inline]
    pub(crate) fn as_virtual_nanos(self) -> u64 {
        self.0
    }

    #[inline]
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.saturating_duration_since(earlier)
    }

    #[inline]
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    #[inline]
    #[must_use]
    pub fn now() -> Self {
        if flash_enabled() {
            Self::now_virtual()
        } else {
            Self::now_real()
        }
    }

    #[inline]
    #[must_use]
    pub(crate) fn now_real() -> Self {
        Self(FLASH.clock.real_now_nanos())
    }

    /// Read the engine clock for platform internals that already selected the
    /// flash branch. Callers use [`Self::now`] to select real or flash time.
    #[inline]
    #[must_use]
    pub(crate) fn now_virtual() -> Self {
        Self(FLASH.clock.now_nanos())
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

impl AddAssign<Duration> for Instant {
    #[inline]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
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
