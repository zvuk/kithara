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
/// real-time thread (the test driver, marked by [`crate::flash::flash_real`])
/// it is a true `tokio` timer; otherwise it registers a virtual deadline on the
/// quiescence engine, so the wait collapses (the clock jumps once all
/// participants park) and consumes no real wall-clock. The decision lives at
/// this single chokepoint — consumers just call `kithara_platform::time::sleep`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub async fn sleep(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::FlashSleep::new(duration).await;
    } else {
        tokio_time::sleep(duration).await;
    }
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(not(all(feature = "flash", not(target_arch = "wasm32"))))]
pub use web_time::Instant;
/// Wall-clock timestamp (native: `std::time::SystemTime`, wasm: `web_time`).
/// Routed through the platform so no crate imports `std::time` directly; unlike
/// [`Instant`] it is not virtualized under `flash` (callers are wall-clock
/// watchdogs/reports that want real time). Use `SystemTime::UNIX_EPOCH` for the
/// epoch anchor.
pub use web_time::SystemTime;

#[cfg(all(feature = "flash", not(target_arch = "wasm32")))]
pub use crate::flash::Instant;

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
    if crate::flash::flash_enabled() {
        FlashTimeout {
            future,
            sleep: crate::flash::FlashSleep::new(duration),
        }
        .await
    } else {
        tokio_time::timeout(duration, future)
            .await
            .map_err(|_| TimeoutError)
    }
}

/// Races `future` against an engine-backed [`crate::flash::FlashSleep`] deadline
/// (see the `flash` [`timeout`]). The future is polled first, so a ready result
/// wins a tie with the deadline. `pub(crate)`: also constructed by the flash
/// control surface's `virtual_timeout`.
#[cfg(all(not(target_arch = "wasm32"), feature = "flash"))]
pub(crate) struct FlashTimeout<F> {
    pub(crate) future: F,
    pub(crate) sleep: crate::flash::FlashSleep,
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
