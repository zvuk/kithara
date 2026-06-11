pub use std::time::Duration;

use tokio_alias::time as tokio_time;
pub use tokio_time::sleep;
use tokio_with_wasm::alias as tokio_alias;
pub use web_time::{Instant, SystemTime};

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
pub async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    tokio_time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}
