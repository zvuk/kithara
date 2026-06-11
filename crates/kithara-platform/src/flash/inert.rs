use crate::time::{Duration, Instant, TimeoutError, sleep, timeout};

/// Off the sim path: spawning needs no quiescence bracket, so `participate` is
/// an identity passthrough (the real clock already advances on its own). Under
/// `flash` this is the engine's `participate`, which wraps the future so it
/// counts in the engine's `active_async` while running.
#[inline]
pub fn participate<F: Future>(fut: F) -> F {
    fut
}

/// Off the sim path: ambient does not exist, so re-asserting it per poll is an
/// identity passthrough. Under `flash` this is the engine's `with_ambient`,
/// which re-establishes the snapshotted ambient around every poll so a future
/// (e.g. the async test body) keeps its flash-eligibility across `.await`
/// thread-hops instead of relying on a one-shot guard set on the first poll.
#[inline]
pub fn with_ambient<F: Future>(_on: bool, fut: F) -> F {
    fut
}

/// No-op real-time scope off the sim path (time is already real).
#[derive(Debug)]
pub struct FlashScope;

/// Enter a real-time scope. Off the sim path this is a ZST no-op; under
/// `flash` it puts the current thread on real time for the guard's lifetime.
#[inline]
#[must_use]
pub fn flash_real() -> FlashScope {
    FlashScope
}

/// Off the sim path: a prod `#[kithara::flash(bool)]` sync region's RAII guard is
/// a ZST no-op (time is already real), so an annotated fn compiles away to its
/// bare body. Under `flash` this is the engine's `enter_dynamic`.
#[inline]
#[must_use]
pub fn enter_dynamic(_on: bool) -> FlashScope {
    FlashScope
}

/// Off the sim path: a prod `#[kithara::flash(bool)]` async region is an identity
/// passthrough (no per-poll re-assert needed when time is already real). Under
/// `flash` this is the engine's `dynamic`.
#[inline]
pub fn dynamic<F: Future>(_on: bool, fut: F) -> F {
    fut
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
/// `#[kithara::test]` macro emits `ambient_scope(..)` into every test body, so
/// the guard must exist (as a ZST) in the off-feature + wasm configs. Under
/// `flash` this is the engine's `AmbientScope` / `ambient_scope`.
#[derive(Debug)]
pub struct AmbientScope;

/// Set the per-test ambient gate. Off the sim path this is a ZST no-op (time is
/// already real); under `flash` it is the engine's `ambient_scope`.
#[inline]
#[must_use]
pub fn ambient_scope(_on: bool) -> AmbientScope {
    AmbientScope
}

/// No engine to report without the `flash` feature (or on wasm).
pub fn dump_to_stderr(_context: &str) {}
