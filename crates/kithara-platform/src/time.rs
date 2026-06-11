#[cfg(target_arch = "wasm32")]
use std::pin::pin;
pub use std::time::Duration;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(target_arch = "wasm32")]
use futures::future::{self as future_util, Either};
#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Promise, Reflect, global};
#[cfg(not(target_arch = "wasm32"))]
use tokio_alias::time as tokio_time;
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub use tokio_time::sleep;
#[cfg(not(target_arch = "wasm32"))]
use tokio_with_wasm::alias as tokio_alias;

/// `sleep` under `flash` (native): real-vs-virtual is decided by the per-
/// thread real-time flag at first poll (the thread that actually awaits). On a
/// real-time thread (the test driver, marked by [`flash_real`]) it is a true
/// `tokio` timer; otherwise it registers a virtual deadline on the quiescence
/// engine, so the wait collapses (the clock jumps once all participants park)
/// and consumes no real wall-clock. The decision lives at this single chokepoint
/// — consumers just call `kithara_platform::time::sleep`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub async fn sleep(duration: Duration) {
    if flash::flash_enabled() {
        flash::FlashSleep::new(duration).await;
    } else {
        tokio_time::sleep(duration).await;
    }
}

/// Render the flash quiescence-engine state (parked participants, deadlines,
/// pending signals). `kithara-test-utils` prints this on a test hang so a
/// wedged run self-reports. Exists only with the engine itself.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use flash::flash_dump_state;
/// Per-thread real-time scope. Under `flash` (native) this is the engine's
/// [`flash::FlashScope`]: while held, [`Instant::now`] reads the real monotonic
/// clock and the synchronous wait primitives use their real implementations, so
/// a real-wall-clock island (the hang watchdog, a real blocking-I/O stretch)
/// coexists with the surrounding virtual clock. Off the sim path it is a ZST
/// no-op (time is already real), so callers hold it through one stable path
/// without a `cfg`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub use flash::{
    AmbientScope, FlashScope, Participating, RealIoScope, ambient_scope, ambient_snapshot,
    enter_dynamic, flash_dynamic, flash_real, participate, real_io, set_ambient_for_spawn,
    with_ambient,
};

/// Virtual `sleep` that hits the quiescence engine UNCONDITIONALLY (no
/// `flash_enabled()` consult). The lexical test rewriter ([`#[kithara::test(flash(true))]`])
/// retargets a test body's direct `time::sleep` calls here, so the BODY's own
/// waits collapse onto virtual time without setting `FLASH_ACTIVE` — a prod fn
/// the body calls keeps its stateless time reads on REAL (`FLASH_ACTIVE` false).
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub fn flash_virtual_sleep(duration: Duration) -> impl Future<Output = ()> {
    flash::FlashSleep::new(duration)
}

/// Virtual `timeout` that hits the engine UNCONDITIONALLY (see
/// [`flash_virtual_sleep`]). Races `future` against an engine-backed deadline.
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub async fn flash_virtual_timeout<F>(
    duration: Duration,
    future: F,
) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    FlashTimeout {
        future,
        sleep: flash::FlashSleep::new(duration),
    }
    .await
}

/// Virtual `Instant::now` read UNCONDITIONALLY from the engine clock (see
/// [`flash_virtual_sleep`]). Mirrors [`flash::Instant::now`]'s flash arm.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
#[must_use]
pub fn flash_virtual_now() -> Instant {
    Instant::now_virtual()
}

/// Virtual `park_timeout` that hits the engine UNCONDITIONALLY (see
/// [`flash_virtual_sleep`]). Mirrors [`crate::thread::park_timeout`]'s flash arm.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub fn flash_virtual_park_timeout(duration: Duration) {
    crate::thread::park_timeout_virtual(duration);
}

/// Off the sim path: spawning needs no quiescence bracket, so `participate` is
/// an identity passthrough (the real clock already advances on its own). Under
/// `flash` this is [`flash::participate`], which wraps the future so it counts
/// in the engine's `active_async` while running.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub fn participate<F: Future>(fut: F) -> F {
    fut
}

/// Off the sim path: ambient does not exist, so re-asserting it per poll is an
/// identity passthrough. Under `flash` this is [`flash::with_ambient`],
/// which re-establishes the snapshotted ambient around every poll so a future
/// (e.g. the async test body) keeps its flash-eligibility across `.await`
/// thread-hops instead of relying on a one-shot guard set on the first poll.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub fn with_ambient<F: Future>(_on: bool, fut: F) -> F {
    fut
}

/// No-op real-time scope off the sim path (time is already real).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[derive(Debug)]
pub struct FlashScope;

/// Enter a real-time scope. Off the sim path this is a ZST no-op; under
/// `flash` it puts the current thread on real time for the guard's lifetime.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
#[must_use]
pub fn flash_real() -> FlashScope {
    FlashScope
}

/// Off the sim path: a prod `#[kithara::flash(bool)]` sync region's RAII guard is
/// a ZST no-op (time is already real), so an annotated fn compiles away to its
/// bare body. Under `flash` this is [`flash::enter_dynamic`].
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
#[must_use]
pub fn enter_dynamic(_on: bool) -> FlashScope {
    FlashScope
}

/// Off the sim path: a prod `#[kithara::flash(bool)]` async region is an identity
/// passthrough (no per-poll re-assert needed when time is already real). Under
/// `flash` this is [`flash::flash_dynamic`].
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub fn flash_dynamic<F: Future>(_on: bool, fut: F) -> F {
    fut
}

/// Off the sim path the lexical test rewriter's `flash_virtual_*` targets alias
/// the REAL primitives, so a rewritten test body behaves identically to its
/// unrewritten form (the rewrite is a no-op when `flash` is off). The
/// `#[kithara::test]` macro emits these into EVERY test body, so they must
/// resolve in the off-feature + wasm configs.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub fn flash_virtual_sleep(duration: Duration) -> impl Future<Output = ()> {
    sleep(duration)
}

/// Off-feature real alias for the rewriter's virtual `timeout` (see
/// [`flash_virtual_sleep`]).
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub async fn flash_virtual_timeout<F>(
    duration: Duration,
    future: F,
) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    timeout(duration, future).await
}

/// Off-feature real alias for the rewriter's virtual `Instant::now` (see
/// [`flash_virtual_sleep`]).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
#[must_use]
pub fn flash_virtual_now() -> Instant {
    Instant::now()
}

/// Off-feature real alias for the rewriter's virtual `park_timeout` (see
/// [`flash_virtual_sleep`]).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
pub fn flash_virtual_park_timeout(duration: Duration) {
    crate::thread::park_timeout(duration);
}

/// Off the sim path a real I/O operation needs no pacing (time is already
/// real), so the scope is a ZST no-op. Under `flash` it is
/// [`flash::RealIoScope`]: while held, the virtual clock may not outrun real
/// time, so virtual watchdogs/timeouts cannot fire spuriously ahead of bytes
/// still on the wire.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[derive(Debug)]
pub struct RealIoScope;

/// Bracket ONE real I/O operation. Off the sim path this is a ZST no-op;
/// under `flash` it paces the virtual clock to real time while held.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
#[must_use]
pub fn real_io() -> RealIoScope {
    RealIoScope
}

/// No-op per-test ambient gate off the sim path (time is already real). The
/// `#[kithara::test]` macro emits `ambient_scope(..)` into every test body, so
/// the guard must exist (as a ZST) in the off-feature + wasm configs. Under
/// `flash` this is [`flash::AmbientScope`] / [`flash::ambient_scope`].
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[derive(Debug)]
pub struct AmbientScope;

/// Set the per-test ambient gate. Off the sim path this is a ZST no-op (time is
/// already real); under `flash` it is [`flash::ambient_scope`].
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash")))]
#[inline]
#[must_use]
pub fn ambient_scope(_on: bool) -> AmbientScope {
    AmbientScope
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

/// Virtual-clock backing for [`Instant`] under the `flash` test feature
/// (native only). Off by default; see the module and crate README.
#[cfg(all(feature = "flash", not(target_arch = "wasm32")))]
pub mod flash;

#[cfg(all(feature = "flash", not(target_arch = "wasm32")))]
pub use flash::Instant;
#[cfg(not(all(feature = "flash", not(target_arch = "wasm32"))))]
pub use web_time::Instant;
/// Wall-clock timestamp (native: `std::time::SystemTime`, wasm: `web_time`).
/// Routed through the platform so no crate imports `std::time` directly; unlike
/// [`Instant`] it is not virtualized under `flash` (callers are wall-clock
/// watchdogs/reports that want real time). Use `SystemTime::UNIX_EPOCH` for the
/// epoch anchor.
pub use web_time::SystemTime;

#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    let ms = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let promise = Promise::new(&mut |resolve, _| {
        let set_timeout: Function = Reflect::get(&global(), &JsValue::from_str("setTimeout"))
            .expect("BUG: setTimeout is a standard browser global, must always exist")
            .unchecked_into();
        let _ = set_timeout.call2(&JsValue::UNDEFINED, &resolve, &JsValue::from(ms));
    });
    let _ = JsFuture::from(promise).await;
}

/// Coarse `Date.now()` timestamp in milliseconds, safe in every wasm scope.
///
/// Unlike [`Instant`] (which reads `performance.now()` and traps in an
/// `AudioWorkletGlobalScope`, where `performance` is undefined), `Date.now()`
/// is valid on the audio render thread. wasm-only: native code with the same
/// need should use [`Instant`] directly, which is cheap there. Coarse
/// (millisecond, wall-clock) — for second-scale deadlines, not precise
/// interval timing.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn coarse_now_ms() -> f64 {
    js_sys::Date::now()
}

/// Error returned when an async operation exceeds its deadline.
#[derive(Debug)]
pub struct TimeoutError;

impl std::fmt::Display for TimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation timed out")
    }
}

impl std::error::Error for TimeoutError {}

/// Await `future` with a deadline that lives on the SAME clock as the awaited
/// work — under `flash` a virtual deadline (collapses with the engine), off
/// it a real `tokio` timer. This is the deadline a PROGRAM imposes on its own
/// async work (e.g. a fetch total-timeout): under sim it must NOT pin the
/// runtime's real timer wheel, or the virtual clock cannot collapse past it.
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash")))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}

/// Await `future` with a deadline on the SAME clock as the awaited work — under
/// `flash` an engine-backed virtual deadline (see the off-feature [`timeout`]
/// for the full contract).
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    if flash::flash_enabled() {
        FlashTimeout {
            future,
            sleep: flash::FlashSleep::new(duration),
        }
        .await
    } else {
        tokio_time::timeout(duration, future)
            .await
            .map_err(|_| TimeoutError)
    }
}

/// Races `future` against an engine-backed [`flash::FlashSleep`] deadline (see the
/// `flash` [`timeout`]). The future is polled first, so a ready result wins a
/// tie with the deadline.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
struct FlashTimeout<F> {
    future: F,
    sleep: flash::FlashSleep,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
impl<F: Future> Future for FlashTimeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `future` and `sleep` are structurally pinned and never moved
        // out of `FlashTimeout`; each is re-pinned in place for its own poll.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: `future` is structurally pinned, never moved out of `FlashTimeout`.
        let fut = unsafe { Pin::new_unchecked(&mut this.future) };
        if let Poll::Ready(out) = fut.poll(cx) {
            return Poll::Ready(Ok(out));
        }
        // SAFETY: `sleep` is structurally pinned, never moved out of `FlashTimeout`.
        let sleep = unsafe { Pin::new_unchecked(&mut this.sleep) };
        match sleep.poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(TimeoutError)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    let deadline = pin!(sleep(duration));
    let work = pin!(future);

    match future_util::select(work, deadline).await {
        Either::Left((output, _)) => Ok(output),
        Either::Right(((), _)) => Err(TimeoutError),
    }
}
