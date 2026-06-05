#[cfg(target_arch = "wasm32")]
use std::pin::pin;
pub use std::time::Duration;
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
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
#[cfg(all(not(target_arch = "wasm32"), not(feature = "sim-time")))]
pub use tokio_time::sleep;
#[cfg(not(target_arch = "wasm32"))]
use tokio_with_wasm::alias as tokio_alias;

/// `sleep` under `sim-time` (native): real-vs-virtual is decided by the per-
/// thread real-time flag at first poll (the thread that actually awaits). On a
/// real-time thread (the test driver, marked by [`real_time`]) it is a true
/// `tokio` timer; otherwise it registers a virtual deadline on the quiescence
/// engine, so the wait collapses (the clock jumps once all participants park)
/// and consumes no real wall-clock. The decision lives at this single chokepoint
/// — consumers just call `kithara_platform::time::sleep`.
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub async fn sleep(duration: Duration) {
    if sim::sim_enabled() {
        sim::SimSleep::new(duration).await;
    } else {
        tokio_time::sleep(duration).await;
    }
}

/// Per-thread real-time scope. Under `sim-time` (native) this is the engine's
/// [`sim::RealTimeScope`]: while held, [`Instant::now`] reads the real monotonic
/// clock and the synchronous wait primitives use their real implementations, so
/// a real-wall-clock island (the hang watchdog, a real blocking-I/O stretch)
/// coexists with the surrounding virtual clock. Off the sim path it is a ZST
/// no-op (time is already real), so callers hold it through one stable path
/// without a `cfg`.
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub use sim::{IoGuard, Participating, RealTimeScope, io_guard, participate, real_time};

/// Off the sim path: spawning needs no quiescence bracket, so `participate` is
/// an identity passthrough (the real clock already advances on its own). Under
/// `sim-time` this is [`sim::participate`], which wraps the future so it counts
/// in the engine's `active_async` while running.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "sim-time")))]
#[inline]
pub fn participate<F: Future>(fut: F) -> F {
    fut
}

/// No-op real-time scope off the sim path (time is already real).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "sim-time")))]
#[derive(Debug)]
pub struct RealTimeScope;

/// Enter a real-time scope. Off the sim path this is a ZST no-op; under
/// `sim-time` it puts the current thread on real time for the guard's lifetime.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "sim-time")))]
#[inline]
#[must_use]
pub fn real_time() -> RealTimeScope {
    RealTimeScope
}

/// No-op real-I/O guard off the sim path (no virtual clock to pace).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "sim-time")))]
#[derive(Debug)]
pub struct IoGuard;

/// Bracket a real I/O operation. Off the sim path this is a ZST no-op; under
/// `sim-time` it paces the virtual clock to the outstanding real fetch for the
/// guard's lifetime, so callers wrap real socket I/O through one stable path.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "sim-time")))]
#[inline]
#[must_use]
pub fn io_guard() -> IoGuard {
    IoGuard
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

/// Virtual-clock backing for [`Instant`] under the `sim-time` test feature
/// (native only). Off by default; see the module and crate README.
#[cfg(all(feature = "sim-time", not(target_arch = "wasm32")))]
pub mod sim;

#[cfg(all(feature = "sim-time", not(target_arch = "wasm32")))]
pub use sim::Instant;
#[cfg(not(all(feature = "sim-time", not(target_arch = "wasm32"))))]
pub use web_time::Instant;
/// Wall-clock timestamp (native: `std::time::SystemTime`, wasm: `web_time`).
/// Routed through the platform so no crate imports `std::time` directly; unlike
/// [`Instant`] it is not virtualized under `sim-time` (callers are wall-clock
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
/// work — under `sim-time` a virtual deadline (collapses with the engine), off
/// it a real `tokio` timer. This is the deadline a PROGRAM imposes on its own
/// async work (e.g. a fetch total-timeout): under sim it must NOT pin the
/// runtime's real timer wheel, or the virtual clock cannot collapse past it. For
/// a wall-clock SAFETY NET that must fire on real time regardless of the virtual
/// clock (the `kithara::test` watchdog), use [`real_timeout`].
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "sim-time")))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}

#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    if sim::sim_enabled() {
        SimTimeout {
            future,
            sleep: sim::SimSleep::new(duration),
        }
        .await
    } else {
        tokio_time::timeout(duration, future)
            .await
            .map_err(|_| TimeoutError)
    }
}

/// Await `future` with a REAL wall-clock deadline, ALWAYS — even under
/// `sim-time`. For wall-clock safety nets (the `kithara::test` timeout watchdog):
/// a hung test hangs real time too, so the deadline must fire on real time and
/// must NOT be collapsed by the virtual clock. Do not use for a program's own
/// async deadlines (use [`timeout`]).
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(not(target_arch = "wasm32"))]
pub async fn real_timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}

/// Races `future` against an engine-backed [`sim::SimSleep`] deadline (see the
/// `sim-time` [`timeout`]). The future is polled first, so a ready result wins a
/// tie with the deadline.
#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
struct SimTimeout<F> {
    future: F,
    sleep: sim::SimSleep,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "sim-time"))]
impl<F: Future> Future for SimTimeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `future` and `sleep` are structurally pinned and never moved
        // out of `SimTimeout`; each is re-pinned in place for its own poll.
        let this = unsafe { self.get_unchecked_mut() };
        let fut = unsafe { Pin::new_unchecked(&mut this.future) };
        if let Poll::Ready(out) = fut.poll(cx) {
            return Poll::Ready(Ok(out));
        }
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

/// On wasm there is no virtual clock, so the wall-clock safety-net deadline is
/// the same `setTimeout`-based race as [`timeout`]. Exists so the
/// `kithara::test` watchdog can call one name across all targets.
#[cfg(target_arch = "wasm32")]
pub async fn real_timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    timeout(duration, future).await
}
