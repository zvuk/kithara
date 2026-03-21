/// Ensure the platform task pool is initialized before browser-side tests run.
///
/// On wasm32 this eagerly touches the blocking task backend once so browser
/// tests do not pay lazy worker-pool setup in the measured path.
#[inline]
pub async fn ensure_thread_pool() {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = super::task::spawn_blocking(|| {}).await;
    }
}
