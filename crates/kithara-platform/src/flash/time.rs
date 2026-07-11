use std::{
    pin::Pin,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;

pub use crate::{
    backend::time::{Duration, SystemTime, TimeoutError},
    flash::Instant,
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
        crate::backend::time::sleep(duration).await;
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
        crate::backend::time::timeout(duration, future).await
    }
}

pin_project! {
    /// Races `future` against an engine-backed [`crate::flash::FlashSleep`] deadline
    /// (see the `flash` [`timeout`]). The future is polled first, so a ready result
    /// wins a tie with the deadline. `pub(crate)`: also constructed by the flash
    /// control surface's `virtual_timeout`.
    pub(crate) struct FlashTimeout<F> {
        #[pin]
        pub(crate) future: F,
        #[pin]
        pub(crate) sleep: crate::flash::FlashSleep,
    }
}

impl<F: Future> Future for FlashTimeout<F> {
    type Output = Result<F::Output, TimeoutError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        // The future is polled FIRST, so a ready result wins a tie with the
        if let Poll::Ready(out) = this.future.poll(cx) {
            return Poll::Ready(Ok(out));
        }
        match this.sleep.poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(TimeoutError)),
            Poll::Pending => Poll::Pending,
        }
    }
}
