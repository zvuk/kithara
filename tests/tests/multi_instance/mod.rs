//! Multi-instance integration tests.
//!
//! Verifies that the library works correctly with multiple concurrent
//! `Audio` instances sharing a rayon `ThreadPool`. Tests 2, 4, and 8
//! instances in various combinations (File, HLS, mixed) and validates
//! that network failures in some instances do not affect others.
//!
//! Thread-lifecycle tests verify that pool threads are released when
//! instances are dropped and the pool remains fully usable afterwards.

use kithara::platform::ThreadPool;

pub(crate) fn test_thread_pool(num_threads: usize) -> ThreadPool {
    if cfg!(target_arch = "wasm32") {
        ThreadPool::global()
    } else {
        ThreadPool::with_num_threads(num_threads).expect("thread pool")
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) const fn available_thread_count(requested: usize) -> usize {
    requested
}

#[cfg(target_arch = "wasm32")]
pub(crate) const fn available_thread_count(_: usize) -> usize {
    1
}

mod concurrent_file;
mod concurrent_hls;
mod concurrent_mixed;
mod failure_resilience;
mod thread_lifecycle;
