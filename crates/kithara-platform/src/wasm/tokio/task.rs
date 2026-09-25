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

/// Spawn a future through the current thread's executor.
///
/// The wasm runtime handle is a compatibility token, so this delegates to
/// [`spawn`], preserving worker lifecycle tracking.
pub fn spawn_on<F, T>(_handle: &Handle, future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    spawn(future)
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
///
/// The returned abort handle stays inert, since blocking work has no yield point to cancel at.
pub fn spawn_blocking<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
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
    abort_handle: futures::future::AbortHandle,
    rx: oneshot::Receiver<Result<T, JoinError>>,
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
#[derive(derive_more::Error)]
#[error(ignore)]
pub struct JoinError {
    #[field(get = is_cancelled)]
    cancelled: bool,
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use kithara_test_utils::kithara;

    use super::{spawn, spawn_on};
    use crate::{
        time::{Duration, sleep},
        tokio::runtime::Handle,
    };

    const TICK: Duration = Duration::from_millis(10);

    #[kithara::test(wasm, flash(false))]
    async fn explicit_handle_preserves_local_future_and_output() {
        let runtime = Handle::try_current().expect("browser executor is available");
        let value = Rc::new(Cell::new(3));
        let input = Rc::clone(&value);
        let joined = spawn_on(&runtime, async move {
            input.set(7);
            input
        })
        .await
        .expect("task completes");
        assert!(Rc::ptr_eq(&value, &joined));
        assert_eq!(value.get(), 7);
    }

    #[kithara::test(wasm, flash(false))]
    async fn abort_stops_the_task_and_the_join_reports_cancelled() {
        for runtime in [None, Some(Handle)] {
            let counter = Rc::new(Cell::new(0u32));
            let ticks = Rc::clone(&counter);

            let future = async move {
                loop {
                    sleep(TICK).await;
                    ticks.set(ticks.get() + 1);
                }
            };
            let handle = match runtime {
                Some(runtime) => spawn_on(&runtime, future),
                None => spawn(future),
            };

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
    }

    #[kithara::test(wasm, flash(false))]
    async fn abort_after_completion_still_yields_the_value() {
        for runtime in [None, Some(Handle)] {
            let future = async { 7u32 };
            let handle = match runtime {
                Some(runtime) => spawn_on(&runtime, future),
                None => spawn(future),
            };

            sleep(TICK * 4).await;
            handle.abort();

            assert_eq!(handle.await.expect("finished task yields its value"), 7);
        }
    }
}
