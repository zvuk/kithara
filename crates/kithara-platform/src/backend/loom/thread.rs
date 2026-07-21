use std::sync::atomic::Ordering;

use crate::{common::thread_id::ACTIVE_NAMED_THREADS, loom::thread as backend};
pub use crate::{
    common::{thread_id::active_named_thread_count, time::Duration},
    loom::thread::JoinHandle,
};

pub type Thread = ::loom::thread::Thread;
pub type ThreadId = ::loom::thread::ThreadId;

fn counted<F, T>(f: F) -> impl FnOnce() -> T + Send + 'static
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    ACTIVE_NAMED_THREADS.fetch_add(1, Ordering::Release);
    move || {
        let result = f();
        ACTIVE_NAMED_THREADS.fetch_sub(1, Ordering::Release);
        result
    }
}

#[derive(Default)]
pub(crate) enum GateBackend {
    #[default]
    Loom,
}

impl GateBackend {
    #[inline]
    pub(crate) fn park(&self) {
        match self {
            Self::Loom => backend::park(),
        }
    }

    #[inline]
    pub(crate) fn park_timeout(&self, duration: Duration) {
        match self {
            Self::Loom => backend::park_timeout(duration),
        }
    }

    #[inline]
    pub(crate) fn unpark(&self, _thread_id: u64, thread: Option<&Thread>) {
        match self {
            Self::Loom => {
                if let Some(thread) = thread {
                    backend::unpark(thread);
                }
            }
        }
    }
}

#[inline]
pub(crate) fn gate_instant(backend: &GateBackend) -> crate::time::Instant {
    match backend {
        GateBackend::Loom => crate::time::Instant::now(),
    }
}

#[inline]
pub fn yield_now() {
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

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    backend::spawn(f)
}

pub fn spawn_named<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    backend::spawn_named_uncounted(name, counted(f))
}

#[track_caller]
pub fn sleep(duration: Duration) {
    backend::sleep(duration);
}

#[track_caller]
pub fn paced_backoff(duration: Duration) {
    let _ = duration;
    backend::yield_now();
}

#[track_caller]
pub fn park() {
    backend::park();
}

#[track_caller]
pub fn park_timeout(duration: Duration) {
    backend::park_timeout(duration);
}

pub fn unpark(thread: &Thread) {
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
