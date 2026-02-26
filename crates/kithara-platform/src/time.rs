//! Platform-aware async time utilities.
//!
//! On native: delegates to [`tokio::time`].
//! On wasm32: `Instant` uses [`web_time`], `sleep` uses `setTimeout`,
//! and `timeout` races the future against a `setTimeout`-based deadline.

pub use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::time::sleep;
pub use web_time::Instant;

#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    use wasm_bindgen::JsCast;

    let ms = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let set_timeout: js_sys::Function = js_sys::Reflect::get(
            &js_sys::global(),
            &wasm_bindgen::JsValue::from_str("setTimeout"),
        )
        .expect("setTimeout must exist")
        .unchecked_into();
        let _ = set_timeout.call2(
            &wasm_bindgen::JsValue::UNDEFINED,
            &resolve,
            &wasm_bindgen::JsValue::from(ms),
        );
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
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

/// Await `future` with a deadline.
///
/// On native: delegates to [`tokio::time::timeout`].
/// On wasm32: races the future against a `setTimeout`-based timer.
///
/// # Errors
///
/// Returns [`TimeoutError`] if the future does not complete within `duration`.
#[cfg(not(target_arch = "wasm32"))]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}

#[cfg(target_arch = "wasm32")]
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    use std::pin::pin;

    use futures::future::{self, Either};

    let deadline = pin!(sleep(duration));
    let work = pin!(future);

    match future::select(work, deadline).await {
        Either::Left((output, _)) => Ok(output),
        Either::Right(((), _)) => Err(TimeoutError),
    }
}
