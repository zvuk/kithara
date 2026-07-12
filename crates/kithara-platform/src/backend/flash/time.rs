use core::future::Future;

pub(crate) use ::tokio::time::sleep;

pub use crate::common::time::{Duration, SystemTime, TimeoutError};

pub(crate) async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, TimeoutError>
where
    F: Future,
{
    ::tokio::time::timeout(duration, future)
        .await
        .map_err(|_| TimeoutError)
}
