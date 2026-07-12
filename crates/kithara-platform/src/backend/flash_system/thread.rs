pub use std::time::Duration;

use crate::common::thread_id::thread_id_hash;

pub type Thread = std::thread::Thread;
pub type ThreadId = std::thread::ThreadId;
pub type JoinHandle<T> = std::thread::JoinHandle<T>;

#[inline]
pub(crate) fn yield_now() {
    std::thread::yield_now();
}

#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    false
}

#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    true
}

#[inline]
pub fn assert_main_thread(_label: &str) {}

#[inline]
pub fn assert_not_main_thread(_label: &str) {}

#[inline]
#[must_use]
pub fn current() -> Thread {
    std::thread::current()
}

pub(crate) fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}

pub(crate) fn spawn_named_uncounted<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("BUG: spawn_named must succeed; thread creation exhausted OS resources")
}

#[track_caller]
pub(crate) fn sleep(duration: Duration) {
    crate::no_block::forbid("thread::sleep");
    std::thread::sleep(duration);
}

#[track_caller]
pub fn park() {
    crate::no_block::forbid("thread::park");
    std::thread::park();
}

#[track_caller]
pub(crate) fn park_timeout(duration: Duration) {
    crate::no_block::forbid("thread::park_timeout");
    std::thread::park_timeout(duration);
}

pub(crate) fn unpark(thread: &Thread) {
    thread.unpark();
}

#[must_use]
pub fn current_thread_id() -> u64 {
    thread_id_hash(current().id())
}

#[must_use]
pub fn available_parallelism() -> Option<core::num::NonZeroUsize> {
    std::thread::available_parallelism().ok()
}
