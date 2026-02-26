//! Shared thread pool for blocking work (decode, probe, I/O).
//!
//! **Native**: workers are spawned once and reuse threads by waiting on a
//! [`Condvar`]-guarded job queue. The global pool is lazily initialized via
//! [`std::sync::OnceLock`].
//!
//! **WASM**: each [`spawn`](ThreadPool::spawn) call creates a fresh Web Worker
//! via [`wasm_safe_thread::spawn`]. A persistent pool is not feasible because
//! `Condvar::wait` / `Atomics.wait` is forbidden on the WASM main thread.
//!
//! # Example
//!
//! ```
//! use kithara_platform::ThreadPool;
//!
//! let pool = ThreadPool::global();
//! pool.spawn(|| { /* CPU work */ });
//! ```

use std::{fmt, io};

use futures::channel::oneshot;

// ── Native: real thread pool ────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
mod inner {
    use std::{collections::VecDeque, io, sync::Arc};

    use wasm_safe_thread::{Mutex, condvar::Condvar};

    type Job = Box<dyn FnOnce() + Send + 'static>;

    pub(super) struct PoolShared {
        queue: Mutex<PoolQueue>,
        condvar: Condvar,
        pub(super) num_threads: usize,
    }

    struct PoolQueue {
        jobs: VecDeque<Job>,
    }

    impl PoolShared {
        pub(super) fn new(num_threads: usize) -> Arc<Self> {
            let shared = Arc::new(Self {
                queue: Mutex::new(PoolQueue {
                    jobs: VecDeque::new(),
                }),
                condvar: Condvar::new(),
                num_threads,
            });

            for _ in 0..num_threads {
                let shared = Arc::clone(&shared);
                let _ = crate::thread::spawn(move || {
                    shared.worker_loop();
                });
            }

            shared
        }

        fn worker_loop(&self) {
            loop {
                let job = {
                    let mut queue = self.queue.lock_sync();
                    loop {
                        if let Some(job) = queue.jobs.pop_front() {
                            break job;
                        }
                        queue = self.condvar.wait_sync(queue);
                    }
                };
                job();
            }
        }

        pub(super) fn submit(&self, job: Job) {
            let mut queue = self.queue.lock_sync();
            queue.jobs.push_back(job);
            self.condvar.notify_one();
        }
    }

    #[derive(Clone)]
    pub(super) struct ThreadPoolInner {
        pub(super) shared: Arc<PoolShared>,
    }

    impl ThreadPoolInner {
        pub(super) fn global() -> Self {
            static POOL: std::sync::OnceLock<ThreadPoolInner> = std::sync::OnceLock::new();
            POOL.get_or_init(|| {
                let n = wasm_safe_thread::available_parallelism()
                    .map(|n| n.get().min(8))
                    .unwrap_or(4);
                Self {
                    shared: PoolShared::new(n),
                }
            })
            .clone()
        }

        pub(super) fn with_num_threads(n: usize) -> Result<Self, io::Error> {
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "thread pool requires at least 1 thread",
                ));
            }
            Ok(Self {
                shared: PoolShared::new(n),
            })
        }

        pub(super) fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
            self.shared.submit(Box::new(f));
        }

        pub(super) fn num_threads(&self) -> usize {
            self.shared.num_threads
        }
    }
}

// ── WASM: spawn-per-task ────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
mod inner {
    use std::io;

    #[derive(Clone)]
    pub(super) struct ThreadPoolInner;

    impl ThreadPoolInner {
        pub(super) fn global() -> Self {
            Self
        }

        pub(super) fn with_num_threads(_n: usize) -> Result<Self, io::Error> {
            Ok(Self)
        }

        pub(super) fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
            let _ = crate::thread::spawn(f);
        }

        pub(super) fn num_threads(&self) -> usize {
            wasm_safe_thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        }
    }
}

/// Shared thread pool for blocking/CPU-bound work.
///
/// On native: wraps a fixed set of worker threads with a shared job queue.
/// On WASM: spawns a fresh Web Worker per task (pool not feasible on main thread).
///
/// # Cloning
///
/// Cloning is cheap. Multiple stream instances should share the same pool.
#[derive(Clone)]
pub struct ThreadPool {
    inner: inner::ThreadPoolInner,
}

impl ThreadPool {
    /// Returns the global thread pool.
    #[must_use]
    pub fn global() -> Self {
        Self {
            inner: inner::ThreadPoolInner::global(),
        }
    }

    /// Create a pool with a specific number of worker threads.
    ///
    /// On native, workers are spawned immediately.
    /// On WASM, `n` is a hint (ignored — tasks spawn fresh workers).
    ///
    /// # Errors
    ///
    /// Returns an error on native if `n` is zero.
    pub fn with_num_threads(n: usize) -> Result<Self, io::Error> {
        Ok(Self {
            inner: inner::ThreadPoolInner::with_num_threads(n)?,
        })
    }

    /// Spawn a closure on the pool (fire-and-forget).
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.inner.spawn(f);
    }

    /// Spawn on the pool and return a future that resolves when the closure completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker thread panics.
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

    /// Returns the number of worker threads in this pool.
    #[must_use]
    pub fn num_threads(&self) -> usize {
        self.inner.num_threads()
    }
}

impl Default for ThreadPool {
    fn default() -> Self {
        Self::global()
    }
}

impl fmt::Debug for ThreadPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadPool")
            .field("num_threads", &self.num_threads())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use super::*;

    #[kithara::test(native)]
    fn test_custom_pool() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        assert_eq!(pool.num_threads(), 2);
    }

    #[kithara::test(native)]
    fn test_zero_threads_is_error() {
        assert!(ThreadPool::with_num_threads(0).is_err());
    }

    #[kithara::test(native)]
    fn test_spawn_completes() {
        let pool = ThreadPool::with_num_threads(2).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        pool.spawn(move || {
            let _ = tx.send(42);
        });
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[kithara::test(native, tokio)]
    #[case::custom(true)]
    #[case::global(false)]
    async fn test_spawn_async(#[case] custom: bool) {
        let pool = if custom {
            ThreadPool::with_num_threads(2).unwrap()
        } else {
            ThreadPool::global()
        };
        let result = pool.spawn_async(|| 42).await.unwrap();
        assert_eq!(result, 42);
    }

    #[kithara::test(native)]
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

    #[kithara::test(wasm)]
    fn test_debug_format() {
        let debug = format!("{:?}", ThreadPool::global());
        assert!(debug.contains("num_threads"));
    }

    #[kithara::test(wasm)]
    #[case::global(false)]
    #[case::default(true)]
    fn test_default_and_global(#[case] default_ctor: bool) {
        let pool = if default_ctor {
            ThreadPool::default()
        } else {
            ThreadPool::global()
        };
        assert!(pool.num_threads() > 0);
    }
}
