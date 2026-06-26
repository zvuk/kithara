pub use tokio_time::sleep;

pub use crate::common::time::{Duration, Instant, SystemTime, TimeoutError};
use crate::native::tokio::backend::time as tokio_time;

/// Await `future` with a real `tokio` timer deadline. This is the deadline a
/// PROGRAM imposes on its own async work (e.g. a fetch total-timeout); every
/// backend keeps it on the same clock as the awaited work.
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
