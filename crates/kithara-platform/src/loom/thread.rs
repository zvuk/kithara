use core::fmt;

use ::loom::sync::{
    Arc as LoomArc,
    atomic::{AtomicBool, Ordering},
};

use crate::common::{thread_id::thread_id_hash, time::Duration};

pub(crate) type Thread = ::loom::thread::Thread;

pub struct JoinHandle<T> {
    finished: LoomArc<AtomicBool>,
    inner: ::loom::thread::JoinHandle<T>,
}

impl<T> JoinHandle<T> {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    delegate::delegate! {
        to self.inner {
            pub fn join(self) -> std::thread::Result<T>;
            #[must_use]
            pub fn thread(&self) -> &Thread;
        }
    }
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("JoinHandle").finish_non_exhaustive()
    }
}

#[inline]
pub(crate) fn yield_now() {
    ::loom::thread::yield_now();
}

#[inline]
#[must_use]
pub(crate) fn is_worker_thread() -> bool {
    false
}

#[inline]
#[must_use]
pub(crate) fn is_main_thread() -> bool {
    true
}

#[inline]
pub(crate) fn assert_main_thread(_label: &str) {}

#[inline]
pub(crate) fn assert_not_main_thread(_label: &str) {}

#[inline]
#[must_use]
pub(crate) fn current() -> Thread {
    ::loom::thread::current()
}

pub(crate) fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (finished, tracked) = tracked(f);
    JoinHandle {
        finished,
        inner: ::loom::thread::spawn(tracked),
    }
}

pub(crate) fn spawn_named_uncounted<F, T, N: Into<String>>(name: N, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (finished, tracked) = tracked(f);
    match ::loom::thread::Builder::new()
        .name(name.into())
        .spawn(tracked)
    {
        Ok(inner) => JoinHandle { finished, inner },
        Err(error) => panic!("failed to spawn named loom thread: {error}"),
    }
}

fn tracked<F, T>(f: F) -> (LoomArc<AtomicBool>, impl FnOnce() -> T)
where
    F: FnOnce() -> T,
{
    let finished = LoomArc::new(AtomicBool::new(false));
    let worker_finished = LoomArc::clone(&finished);
    let run = move || {
        let _finished = Finished(worker_finished);
        f()
    };
    (finished, run)
}

struct Finished(LoomArc<AtomicBool>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[track_caller]
pub(crate) fn sleep(duration: Duration) {
    panic!("thread::sleep({duration:?}) requires the flash backend when loom is enabled");
}

#[inline]
#[track_caller]
pub(crate) fn park() {
    crate::no_block::forbid("thread::park");
    ::loom::thread::park();
}

#[track_caller]
pub(crate) fn park_timeout(duration: Duration) {
    panic!("thread::park_timeout({duration:?}) requires flash when loom is enabled");
}

#[inline]
pub(crate) fn unpark(thread: &Thread) {
    thread.unpark();
}

#[inline]
#[must_use]
pub(crate) fn current_thread_id() -> u64 {
    thread_id_hash(current().id())
}

#[inline]
#[must_use]
pub(crate) fn available_parallelism() -> Option<core::num::NonZeroUsize> {
    core::num::NonZeroUsize::new(1)
}
