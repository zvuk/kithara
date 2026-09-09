use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    channel::oneshot,
    future::{Aborted, abortable},
};

pub use super::backend::task::*;
use super::{backend::task as tww_task, runtime::Handle};
use crate::maybe_send::MaybeSend;

/// Forward a `tokio_with_wasm` `JoinHandle` into our own oneshot-backed
/// channel, collapsing an abort and a tww join error into our `JoinError`.
fn forward_tww_handle<H, T>(tx: oneshot::Sender<Result<T, JoinError>>, handle: H)
where
    H: Future<Output = Result<Result<T, Aborted>, tww_task::JoinError>> + 'static,
    T: 'static,
{
    wasm_bindgen_futures::spawn_local(async move {
        let result = match handle.await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(Aborted)) | Err(_) => Err(JoinError { cancelled: true }),
        };
        drop(tx.send(result));
    });
}

/// Spawn an async task on the current thread's executor.
///
/// On a Web Worker, wraps with `task_begin`/`task_finished` lifecycle hooks.
/// On main thread, delegates to `tokio_with_wasm`.
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let (tx, rx) = oneshot::channel();
    let (future, abort_handle) = abortable(future);

    if crate::thread::is_worker_thread() {
        wasm_safe_thread::task_begin();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match future.await {
                Ok(val) => Ok(val),
                Err(Aborted) => Err(JoinError { cancelled: true }),
            };
            drop(tx.send(result));
            wasm_safe_thread::task_finished();
        });
    } else {
        forward_tww_handle(tx, tww_task::spawn(future));
    }

    JoinHandle { rx, abort_handle }
}

/// Spawn synchronous work after yielding the current async step.
pub fn spawn_sync<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + MaybeSend + 'static,
    T: MaybeSend + 'static,
{
    spawn(async move { f() })
}

/// Run a blocking closure on a dedicated Web Worker thread.
///
/// On a Web Worker, spawns via `crate::thread::spawn`.
/// On main thread, delegates to `tokio_with_wasm`'s worker pool.
pub fn spawn_blocking<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    // Blocking work has no yield point, so its abort handle stays inert.
    let (abort_handle, _registration) = futures::future::AbortHandle::new_pair();

    if crate::thread::is_worker_thread() {
        drop(crate::thread::spawn(move || {
            drop(tx.send(Ok(f())));
        }));
    } else {
        forward_tww_handle(tx, tww_task::spawn_blocking(move || Ok(f())));
    }

    JoinHandle { rx, abort_handle }
}

/// Run a blocking closure on a dedicated Web Worker thread.
///
/// The wasm runtime handle is a compatibility token, so this delegates to
/// [`spawn_blocking`].
pub fn spawn_blocking_on<F, T>(_handle: &Handle, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking(f)
}

/// Handle to a spawned async task.
pub struct JoinHandle<T> {
    rx: oneshot::Receiver<Result<T, JoinError>>,
    abort_handle: futures::future::AbortHandle,
}

impl<T> JoinHandle<T> {
    /// Stop the task at its next yield point; the join then reports cancelled.
    ///
    /// A finished task keeps its value, and blocking work runs to completion.
    pub fn abort(&self) {
        self.abort_handle.abort();
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(JoinError { cancelled: true })),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Error returned when a spawned task fails.
#[derive(Debug, derive_more::Display, fieldwork::Fieldwork)]
#[display("task failed to execute to completion")]
#[fieldwork(get)]
pub struct JoinError {
    #[field(get = is_cancelled)]
    cancelled: bool,
}

impl std::error::Error for JoinError {}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use kithara_test_utils::kithara;

    use super::spawn;
    use crate::time::{Duration, sleep};

    const TICK: Duration = Duration::from_millis(10);

    #[kithara::test(wasm, flash(false))]
    async fn abort_stops_the_task_and_the_join_reports_cancelled() {
        let counter = Rc::new(Cell::new(0u32));
        let ticks = Rc::clone(&counter);

        let handle = spawn(async move {
            loop {
                sleep(TICK).await;
                ticks.set(ticks.get() + 1);
            }
        });

        while counter.get() == 0 {
            sleep(TICK).await;
        }

        handle.abort();
        let stopped_at = counter.get();
        sleep(TICK * 4).await;

        assert_eq!(counter.get(), stopped_at);
        let err = handle.await.expect_err("aborted task joins with an error");
        assert!(err.is_cancelled());
    }

    #[kithara::test(wasm, flash(false))]
    async fn abort_after_completion_still_yields_the_value() {
        let handle = spawn(async { 7u32 });

        sleep(TICK * 4).await;
        handle.abort();

        assert_eq!(handle.await.expect("finished task yields its value"), 7);
    }
}
