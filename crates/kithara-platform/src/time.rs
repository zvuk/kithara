#[cfg(target_arch = "wasm32")]
use std::pin::pin;
pub use std::time::Duration;
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
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
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
pub use tokio_time::sleep;
#[cfg(not(target_arch = "wasm32"))]
use tokio_with_wasm::alias as tokio_alias;

/// `sleep` under `flash-time` (native): real-vs-virtual is decided by the per-
/// thread real-time flag at first poll (the thread that actually awaits). On a
/// real-time thread (the test driver, marked by [`flash_real`]) it is a true
/// `tokio` timer; otherwise it registers a virtual deadline on the quiescence
/// engine, so the wait collapses (the clock jumps once all participants park)
/// and consumes no real wall-clock. The decision lives at this single chokepoint
/// — consumers just call `kithara_platform::time::sleep`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub async fn sleep(duration: Duration) {
    if flash::flash_enabled() {
        flash::FlashSleep::new(duration).await;
    } else {
        tokio_time::sleep(duration).await;
    }
}

/// Per-thread real-time scope. Under `flash-time` (native) this is the engine's
/// [`flash::FlashScope`]: while held, [`Instant::now`] reads the real monotonic
/// clock and the synchronous wait primitives use their real implementations, so
/// a real-wall-clock island (the hang watchdog, a real blocking-I/O stretch)
/// coexists with the surrounding virtual clock. Off the sim path it is a ZST
/// no-op (time is already real), so callers hold it through one stable path
/// without a `cfg`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
pub use flash::{
    AmbientScope, FlashScope, Participating, ambient_scope, ambient_snapshot, enter_dynamic,
    flash_real, participate, set_ambient_for_spawn,
};

/// Off the sim path: spawning needs no quiescence bracket, so `participate` is
/// an identity passthrough (the real clock already advances on its own). Under
/// `flash-time` this is [`flash::participate`], which wraps the future so it counts
/// in the engine's `active_async` while running.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash-time")))]
#[inline]
pub fn participate<F: Future>(fut: F) -> F {
    fut
}

/// No-op real-time scope off the sim path (time is already real).
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash-time")))]
#[derive(Debug)]
pub struct FlashScope;

/// Enter a real-time scope. Off the sim path this is a ZST no-op; under
/// `flash-time` it puts the current thread on real time for the guard's lifetime.
#[cfg(not(all(not(target_arch = "wasm32"), feature = "flash-time")))]
#[inline]
#[must_use]
pub fn flash_real() -> FlashScope {
    FlashScope
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

/// Virtual-clock backing for [`Instant`] under the `flash-time` test feature
/// (native only). Off by default; see the module and crate README.
#[cfg(all(feature = "flash-time", not(target_arch = "wasm32")))]
pub mod flash;

#[cfg(all(feature = "flash-time", not(target_arch = "wasm32")))]
pub use flash::Instant;
#[cfg(not(all(feature = "flash-time", not(target_arch = "wasm32"))))]
pub use web_time::Instant;
/// Wall-clock timestamp (native: `std::time::SystemTime`, wasm: `web_time`).
/// Routed through the platform so no crate imports `std::time` directly; unlike
/// [`Instant`] it is not virtualized under `flash-time` (callers are wall-clock
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
/// work — under `flash-time` a virtual deadline (collapses with the engine), off
/// it a real `tokio` timer. This is the deadline a PROGRAM imposes on its own
/// async work (e.g. a fetch total-timeout): under sim it must NOT pin the
/// runtime's real timer wheel, or the virtual clock cannot collapse past it.
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "flash-time")))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
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
/// `flash-time` [`timeout`]). The future is polled first, so a ready result wins a
/// tie with the deadline.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
struct FlashTimeout<F> {
    future: F,
    sleep: flash::FlashSleep,
}

#[cfg(all(not(target_arch = "wasm32"), feature = "flash-time"))]
impl<F: Future> Future for FlashTimeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `future` and `sleep` are structurally pinned and never moved
        // out of `FlashTimeout`; each is re-pinned in place for its own poll.
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
