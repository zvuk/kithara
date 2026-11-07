/// Dedicated browser workers need no shared pool initialization.
#[inline]
pub async fn ensure_thread_pool() {}
