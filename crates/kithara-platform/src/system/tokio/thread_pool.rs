/// Ensure the platform task pool is initialized before browser-side tests run.
///
/// Native no-op: the tokio blocking pool needs no eager warm-up.
#[inline]
pub async fn ensure_thread_pool() {}
