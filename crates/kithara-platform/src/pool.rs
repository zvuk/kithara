//! Shared thread pool for blocking work (decode, probe, I/O).
//!
//! Wraps [`rayon::ThreadPool`] to provide a consistent threading abstraction
//! across all Kithara crates. Pass through configs to share a single pool
//! across multiple stream instances.
//!
//! By default, the global rayon pool is used. Create a custom pool with
//! [`ThreadPool::with_num_threads`] or [`ThreadPool::custom`] to control
//! thread count and lifecycle.
//!
//! # WASM support
//!
//! Rayon supports WASM via `wasm-bindgen-rayon`, mapping pool threads
//! to Web Workers. This makes `ThreadPool` portable across native and web.

use std::{fmt, io, sync::Arc};

use futures::channel::oneshot;
/// Shared thread pool for blocking/CPU-bound work.
///
/// Wraps an optional [`rayon::ThreadPool`]. When `None`, delegates to the
/// global rayon pool.
///
/// # Cloning
///
/// Cloning is cheap (Arc increment). Multiple stream instances should share
/// the same pool to avoid uncontrolled thread proliferation.
///
/// # Example
///
/// ```
/// use kithara_platform::ThreadPool;
///
/// // Use global rayon pool (default)
/// let pool = ThreadPool::global();
///
/// // Custom pool with 4 threads
/// let pool = ThreadPool::with_num_threads(4).unwrap();
///
/// // Fire-and-forget
/// pool.spawn(|| { /* CPU work */ });
/// ```
#[derive(Clone)]
pub struct ThreadPool {
    inner: Option<Arc<rayon::ThreadPool>>,
}

impl ThreadPool {
    /// Use the global rayon thread pool.
    ///
    /// The global pool is created lazily on first use with `num_cpus` threads.
    /// This is the default.
    #[must_use]
    pub fn global() -> Self {
        Self { inner: None }
    }

    /// Wrap an existing [`rayon::ThreadPool`].
    ///
    /// When the last clone of `ThreadPool` is dropped and no tasks are running,
    /// the underlying rayon pool is destroyed and its threads exit.
    #[must_use]
    pub fn custom(pool: rayon::ThreadPool) -> Self {
        Self {
            inner: Some(Arc::new(pool)),
        }
    }

    /// Create a pool with a specific number of threads.
    ///
    /// # Errors
    ///
    /// Returns an error if the rayon pool cannot be created.
    pub fn with_num_threads(n: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build()?;
        Ok(Self::custom(pool))
    }

    /// Spawn a closure on the pool (fire-and-forget).
    ///
    /// The closure runs on a pool thread. Use [`spawn_async`](Self::spawn_async)
    /// to get a future that resolves when the closure completes.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        match self.inner {
            Some(ref pool) => pool.spawn(f),
            None => rayon::spawn(f),
        }
    }

    /// Spawn on the pool and return a future that resolves when the closure completes.
    ///
    /// Replaces `tokio::task::spawn_blocking` — offloads blocking work from
    /// an async context to a rayon pool thread without tying up the tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool thread panics.
    pub async fn spawn_async<F, R>(&self, f: F) -> Result<R, io::Error>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.spawn(move || {
            let _ = tx.send(f());
        });
        rx.await
            .map_err(|_| io::Error::other("thread pool task panicked"))
    }
}

impl Default for ThreadPool {
    fn default() -> Self {
        Self::global()
    }
}

impl fmt::Debug for ThreadPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.inner {
            Some(ref pool) => f
                .debug_struct("ThreadPool")
                .field("kind", &"custom")
                .field("num_threads", &pool.current_num_threads())
                .finish(),
            None => f
                .debug_struct("ThreadPool")
                .field("kind", &"global")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_pool() {
        let pool = ThreadPool::global();
        assert!(pool.inner.is_none());
    }

    #[test]
    fn test_custom_pool() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        assert!(pool.inner.is_some());
    }

    #[test]
    fn test_spawn_completes() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        pool.spawn(move || {
            let _ = tx.send(42);
        });
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[tokio::test]
    async fn test_spawn_async() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        let result = pool.spawn_async(|| 42).await.unwrap();
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_spawn_async_global() {
        let pool = ThreadPool::global();
        let result = pool.spawn_async(|| 42).await.unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_clone_shares_pool() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        let pool2 = pool.clone();

        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        pool.spawn(move || {
            let _ = tx1.send(1);
        });
        pool2.spawn(move || {
            let _ = tx2.send(2);
        });
        assert_eq!(rx1.recv().unwrap(), 1);
        assert_eq!(rx2.recv().unwrap(), 2);
    }

    #[test]
    fn test_debug_global() {
        let pool = ThreadPool::global();
        let debug = format!("{:?}", pool);
        assert!(debug.contains("global"));
    }

    #[test]
    fn test_debug_custom() {
        let pool = ThreadPool::with_num_threads(3).unwrap();
        let debug = format!("{:?}", pool);
        assert!(debug.contains("custom"));
    }

    #[test]
    fn test_default_is_global() {
        let pool = ThreadPool::default();
        assert!(pool.inner.is_none());
    }
}
