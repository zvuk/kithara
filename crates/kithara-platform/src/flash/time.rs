use std::{
    pin::Pin,
    task::{Context, Poll},
};

use tokio_alias::time as tokio_time;
use tokio_with_wasm::alias as tokio_alias;

pub use crate::{
    flash::Instant,
    native::time::{Duration, SystemTime, TimeoutError},
};

/// `sleep` under `flash` (native): real-vs-virtual is decided by the per-
/// thread real-time flag at first poll (the thread that actually awaits). On a
/// real-time thread (the test driver, marked by [`crate::flash::flash_real`])
/// it is a true `tokio` timer; otherwise it registers a virtual deadline on the
/// quiescence engine, so the wait collapses (the clock jumps once all
/// participants park) and consumes no real wall-clock. The decision lives at
/// this single chokepoint — consumers just call `kithara_platform::time::sleep`.
pub async fn sleep(duration: Duration) {
    if crate::flash::flash_enabled() {
        crate::flash::FlashSleep::new(duration).await;
    } else {
        tokio_time::sleep(duration).await;
    }
}

/// Await `future` with a deadline on the SAME clock as the awaited work — under
/// `flash` an engine-backed virtual deadline (see the off-feature [`timeout`]
/// for the full contract).
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
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
pub(crate) struct FlashTimeout<F> {
    pub(crate) future: F,
    pub(crate) sleep: crate::flash::FlashSleep,
}

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
