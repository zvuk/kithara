use crate::loom::thread as backend;
pub use crate::{common::time::Duration, loom::thread::JoinHandle};

pub type Thread = ::loom::thread::Thread;
pub type ThreadId = ::loom::thread::ThreadId;

#[inline]
pub(crate) fn yield_now() {
    backend::yield_now();
}

#[inline]
#[must_use]
pub fn is_worker_thread() -> bool {
    backend::is_worker_thread()
}

#[inline]
#[must_use]
pub fn is_main_thread() -> bool {
    backend::is_main_thread()
}

#[inline]
pub fn assert_main_thread(label: &str) {
    backend::assert_main_thread(label);
}

#[inline]
pub fn assert_not_main_thread(label: &str) {
    backend::assert_not_main_thread(label);
}

#[inline]
#[must_use]
pub fn current() -> Thread {
    backend::current()
}

pub(crate) fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    backend::spawn(f)
}

pub(crate) fn spawn_named_uncounted<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    backend::spawn_named_uncounted(name, f)
}

#[track_caller]
pub(crate) fn sleep(duration: Duration) {
    backend::sleep(duration);
}

#[track_caller]
pub fn park() {
    backend::park();
}

#[track_caller]
pub(crate) fn park_timeout(duration: Duration) {
    backend::park_timeout(duration);
}

pub(crate) fn unpark(thread: &Thread) {
    backend::unpark(thread);
}

#[must_use]
pub fn current_thread_id() -> u64 {
    backend::current_thread_id()
}

#[must_use]
pub fn available_parallelism() -> Option<core::num::NonZeroUsize> {
    backend::available_parallelism()
}
