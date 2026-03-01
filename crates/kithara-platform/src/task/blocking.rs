//! Cross-platform [`spawn_blocking`] that returns a handle supporting
//! `.await` and `.is_finished()`.
//!
//! * **Native** — delegates to [`tokio::task::spawn_blocking`].
//! * **WASM** — sends the closure to a [`wasm_safe_thread`] Web Worker and
//!   returns the result through a [`futures::channel::oneshot`] channel.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

// ── Native ──────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_blocking<F, T>(f: F) -> BlockingHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    BlockingHandle(tokio::task::spawn_blocking(f))
}

#[cfg(not(target_arch = "wasm32"))]
pub struct BlockingHandle<T>(tokio::task::JoinHandle<T>);

#[cfg(not(target_arch = "wasm32"))]
impl<T> BlockingHandle<T> {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> Future for BlockingHandle<T> {
    type Output = Result<T, BlockingError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // JoinHandle is Unpin, so Pin::new is safe
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|r| r.map_err(|_| BlockingError))
    }
}

// ── WASM ────────────────────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
pub fn spawn_blocking<F, T>(f: F) -> BlockingHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let (tx, rx) = futures::channel::oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let finished2 = Arc::clone(&finished);
    let _ = crate::thread::spawn(move || {
        let _ = tx.send(f());
        finished2.store(true, Ordering::Release);
    });
    BlockingHandle { rx, finished }
}

#[cfg(target_arch = "wasm32")]
pub struct BlockingHandle<T> {
    rx: futures::channel::oneshot::Receiver<T>,
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(target_arch = "wasm32")]
impl<T> BlockingHandle<T> {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(target_arch = "wasm32")]
impl<T> Future for BlockingHandle<T> {
    type Output = Result<T, BlockingError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: oneshot::Receiver is Unpin
        match Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(val)) => Poll::Ready(Ok(val)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(BlockingError)),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ── Shared error type ───────────────────────────────────────────────

/// The blocking task panicked or was cancelled.
#[derive(Debug)]
pub struct BlockingError;

impl std::fmt::Display for BlockingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("blocking task failed")
    }
}

impl std::error::Error for BlockingError {}
