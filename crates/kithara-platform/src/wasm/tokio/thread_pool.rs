/// Initialize the browser blocking task pool on the main thread without
/// spawning a nested worker from dedicated-worker tests.
#[inline]
pub async fn ensure_thread_pool() {
    if crate::thread::is_main_thread() {
        let _ = super::task::spawn_blocking(|| {}).await;
    }
}
