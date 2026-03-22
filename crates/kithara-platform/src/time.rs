//! Platform-aware async time utilities.
//!
//! On native: delegates to [`tokio::time`].
//! On wasm32: `Instant` uses [`web_time`], `sleep` uses `setTimeout`,
//! and `timeout` races the future against a `setTimeout`-based deadline.

#[cfg(target_arch = "wasm32")]
use std::pin::pin;
pub use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use futures::future::{self as future_util, Either};
#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Promise, Reflect, global};
#[cfg(not(target_arch = "wasm32"))]
use tokio_alias::time as tokio_time;
#[cfg(not(target_arch = "wasm32"))]
pub use tokio_time::sleep;
#[cfg(not(target_arch = "wasm32"))]
use tokio_with_wasm::alias as tokio_alias;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
pub use web_time::Instant;

#[cfg(target_arch = "wasm32")]
pub async fn sleep(duration: Duration) {
    let ms = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let promise = Promise::new(&mut |resolve, _| {
        let set_timeout: Function = Reflect::get(&global(), &JsValue::from_str("setTimeout"))
            .expect("setTimeout must exist")
            .unchecked_into();
        let _ = set_timeout.call2(&JsValue::UNDEFINED, &resolve, &JsValue::from(ms));
    });
    let _ = JsFuture::from(promise).await;
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
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
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
