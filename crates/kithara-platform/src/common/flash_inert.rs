pub use std::time::Duration;
use std::{
    marker::PhantomData,
    panic::Location,
    pin::Pin,
    task::{Context, Poll},
};

pub use crate::time::Instant;
use crate::{
    common::time::TimeoutError,
    time::{sleep, timeout},
};

/// Off the sim path a spawned task needs no quiescence accounting, so the
/// participant wrapper is the future itself. Under `flash` this is the
/// engine's gate wrapper counted in `active_async`.
pub type Participating<F> = F;

/// Off the sim path: spawning needs no quiescence bracket, so `participate` is
/// an identity passthrough (the real clock already advances on its own). Under
/// `flash` this is the engine's `participate`, which wraps the future so it
/// counts in the engine's `active_async` while running. The `loc` spawn-site
/// identity (used by the engine's hang dump) is unused off the sim path.
#[inline]
pub fn participate<F: Future>(fut: F, _loc: &'static Location<'static>) -> Participating<F> {
    fut
}

/// Off the sim path ambient does not exist, so there is nothing to re-assert
/// per poll: the wrapper is the future itself. Under `flash` this is the
/// engine's per-poll ambient re-assert wrapper.
pub type WithAmbient<F> = F;

/// Off the sim path: ambient does not exist, so re-asserting it per poll is an
/// identity passthrough. Under `flash` this is the engine's `with_ambient`,
/// which re-establishes the snapshotted ambient around every poll so a future
/// (e.g. the async test body) keeps its flash-eligibility across `.await`
/// thread-hops instead of relying on a one-shot guard set on the first poll.
#[inline]
pub fn with_ambient<F: Future>(_on: bool, fut: F) -> WithAmbient<F> {
    fut
}

/// No-op real-time scope off the sim path (time is already real). `!Send`
/// (`PhantomData<*mut ()>`) for auto-trait PARITY with the engine guard: a
/// consumer compiling against the inert form must not become Send-legal code
/// that fails to compile under `flash`.
#[derive(Debug)]
pub struct FlashScope {
    _not_send: PhantomData<*mut ()>,
}

/// Enter a real-time scope. Off the sim path this is a ZST no-op; under
/// `flash` it puts the current thread on real time for the guard's lifetime.
#[inline]
#[must_use]
pub fn flash_real() -> FlashScope {
    FlashScope {
        _not_send: PhantomData,
    }
}

/// Off the sim path: a prod `#[kithara::flash(bool)]` sync region's RAII guard is
/// a ZST no-op (time is already real), so an annotated fn compiles away to its
/// bare body. Under `flash` this is the engine's `enter_dynamic`.
#[inline]
#[must_use]
pub fn enter_dynamic(_on: bool) -> FlashScope {
    FlashScope {
        _not_send: PhantomData,
    }
}

/// Off the sim path a prod async `#[kithara::flash(bool)]` region needs no
/// per-poll mode re-assert: the wrapper is the future itself. Under `flash`
/// this is the engine's per-poll dynamic-flash wrapper.
pub type FlashDynamic<F> = F;

/// Off the sim path: a prod `#[kithara::flash(bool)]` async region is an identity
/// passthrough (no per-poll re-assert needed when time is already real). Under
/// `flash` this is the engine's `dynamic`.
#[inline]
pub fn dynamic<F: Future>(_on: bool, fut: F) -> FlashDynamic<F> {
    fut
}

/// Inert mirror of the engine-backed yield future. Never produced by the inert
/// [`yield_now`] (off the sim path the real arm is always taken); present so
/// the surface matches the flash control surface 1:1.
pub struct FlashYield {
    _priv: (),
}

impl Future for FlashYield {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}

/// Cooperative async yield. Off the sim path this always takes the real arm —
/// the same semantics the engine build uses outside a flash-eligible test:
/// hand control back to the scheduler once, then resolve.
pub fn yield_now() -> Yield {
    Yield::Real { yielded: false }
}

/// Cooperative yield future (see [`yield_now`]). Mirrors the flash enum; the
/// [`Yield::Flash`] variant is never constructed off the sim path.
#[must_use = "a Yield future does nothing unless `.await`ed"]
pub enum Yield {
    /// Engine-backed quiescence yield: surface parity only, never built here.
    Flash(FlashYield),
    /// Real cooperative yield: returns `Pending` once after re-arming the
    /// waker, then `Ready` — the same hand-back-to-the-scheduler semantics as
    /// `tokio::task::yield_now`.
    Real { yielded: bool },
}

impl Future for Yield {
    type Output = ();

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

/// Off the sim path the lexical test rewriter's `virtual_*` targets alias
/// the REAL primitives, so a rewritten test body behaves identically to its
/// unrewritten form (the rewrite is a no-op when `flash` is off). The
/// `#[kithara::test]` macro emits these into EVERY test body, so they must
/// resolve in the off-feature + wasm configs.
#[inline]
pub fn virtual_sleep(duration: Duration) -> impl Future<Output = ()> {
    sleep(duration)
}

/// Off-feature real alias for the rewriter's virtual `timeout` (see
/// [`virtual_sleep`]).
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[inline]
pub async fn virtual_timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    timeout(duration, future).await
}

/// Off-feature real alias for the rewriter's virtual `Instant::now` (see
/// [`virtual_sleep`]).
#[inline]
#[must_use]
pub fn virtual_now() -> Instant {
    Instant::now()
}

/// Off-feature real alias for the rewriter's virtual `park_timeout` (see
/// [`virtual_sleep`]).
#[inline]
pub fn virtual_park_timeout(duration: Duration) {
    crate::thread::park_timeout(duration);
}

/// No virtual timeline or quiescence engine to reset off the sim path (the
/// clock is real), so this is a no-op.
#[inline]
pub fn reset() {}

/// Off the sim path a real I/O operation needs no pacing (time is already
/// real), so the scope is a ZST no-op. Under `flash` it is the engine's
/// `RealIoScope`: while held, the virtual clock may not outrun real
/// time, so virtual watchdogs/timeouts cannot fire spuriously ahead of bytes
/// still on the wire.
#[derive(Debug)]
pub struct RealIoScope;

/// Bracket ONE real I/O operation. Off the sim path this is a ZST no-op;
/// under `flash` it paces the virtual clock to real time while held.
#[inline]
#[must_use]
pub fn real_io() -> RealIoScope {
    RealIoScope
}

/// No-op per-test ambient gate off the sim path (time is already real). The
/// `#[kithara::test]` macro emits `ambient_scope(..)` into sync and wasm test
/// bodies (async-native bodies carry `with_ambient` per-poll instead), so the
/// guard must exist (as a ZST) in the off-feature + wasm configs. Under
/// `flash` this is the engine's `AmbientScope` / `ambient_scope`. `!Send` for
/// auto-trait parity with the engine guard (see [`FlashScope`]).
#[derive(Debug)]
pub struct AmbientScope {
    _not_send: PhantomData<*mut ()>,
}

/// Set the per-test ambient gate. Off the sim path this is a ZST no-op (time is
/// already real); under `flash` it is the engine's `ambient_scope`.
#[inline]
#[must_use]
pub fn ambient_scope(_on: bool) -> AmbientScope {
    AmbientScope {
        _not_send: PhantomData,
    }
}

/// Snapshot the per-test ambient gate for spawn propagation. Off the sim path
/// the gate does not exist and no test is flash-eligible, so the snapshot is
/// always `false` (the engine's default outside a flash test).
#[inline]
#[must_use]
pub fn ambient_snapshot() -> bool {
    false
}

/// Restore a snapshotted ambient on a spawned child. Off the sim path this is
/// the ZST no-op guard (see [`ambient_scope`]); under `flash` it re-establishes
/// the engine's per-test gate for the child's lifetime.
#[inline]
#[must_use]
pub fn set_ambient_for_spawn(_on: bool) -> AmbientScope {
    AmbientScope {
        _not_send: PhantomData,
    }
}

/// No engine to report without the `flash` feature (or on wasm).
#[must_use]
pub fn hang_dump(_context: &str) -> String {
    String::new()
}

/// No engine to report without the `flash` feature (or on wasm).
pub fn log_hang_dump(_context: &str) {}
