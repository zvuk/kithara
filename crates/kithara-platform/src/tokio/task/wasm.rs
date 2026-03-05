//! WASM task spawning with Web Worker lifecycle dispatch.
//!
//! - Main thread: delegates to `tokio_with_wasm` (standard WASM tokio shim).
//! - Web Worker: `spawn_local` + `task_begin`/`task_finished` to prevent
//!   premature Worker shutdown.
//!
//! Custom `JoinHandle`/`JoinError` are necessary because `tokio_with_wasm`'s
//! `JoinHandle` has private fields and their `spawn`/`spawn_blocking` panic
//! on Web Workers.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Spawn an async task on the current thread's executor.
///
/// On a Web Worker, wraps with `task_begin`/`task_finished` lifecycle hooks.
/// On main thread, delegates to `tokio_with_wasm`.
pub fn spawn<F, T>(future: F) -> JoinHandle<T>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    let (tx, rx) = futures::channel::oneshot::channel();

    if crate::thread::is_worker_thread() {
        wasm_safe_thread::task_begin();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = tx.send(Ok(future.await));
            wasm_safe_thread::task_finished();
        });
    } else {
        let handle = tokio_with_wasm::alias::task::spawn(future);
        wasm_bindgen_futures::spawn_local(async move {
            match handle.await {
                Ok(val) => {
                    let _ = tx.send(Ok(val));
                }
                Err(_) => {
                    let _ = tx.send(Err(JoinError { cancelled: true }));
                }
            }
        });
    }

    JoinHandle { rx }
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
    let (tx, rx) = futures::channel::oneshot::channel();

    if crate::thread::is_worker_thread() {
        let _ = crate::thread::spawn(move || {
            let _ = tx.send(Ok(f()));
        });
    } else {
        let handle = tokio_with_wasm::alias::task::spawn_blocking(f);
        wasm_bindgen_futures::spawn_local(async move {
            match handle.await {
                Ok(val) => {
                    let _ = tx.send(Ok(val));
                }
                Err(_) => {
                    let _ = tx.send(Err(JoinError { cancelled: true }));
                }
            }
        });
    }

    JoinHandle { rx }
}

/// Handle to a spawned async task.
pub struct JoinHandle<T> {
    rx: futures::channel::oneshot::Receiver<Result<T, JoinError>>,
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
#[derive(Debug)]
pub struct JoinError {
    cancelled: bool,
}

impl JoinError {
    /// Returns `true` if the task was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("task failed to execute to completion")
    }
}

impl std::error::Error for JoinError {}
