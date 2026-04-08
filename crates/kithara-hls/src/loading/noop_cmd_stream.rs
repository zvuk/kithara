#![forbid(unsafe_code)]

//! Noop `Stream<Item = FetchCmd>` used to obtain a [`TrackHandle`] for
//! the HLS path.
//!
//! HLS does not yield commands through a protocol stream — every fetch
//! is dispatched directly via [`TrackHandle::execute*()`]. But
//! [`Downloader::register`] is the only public way to obtain a
//! `TrackHandle`, so HLS registers a stream that stays `Pending` until
//! its own cancel token fires, then completes (`Poll::Ready(None)`)
//! and is removed from the `SelectAll` set inside the downloader loop.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Stream, future::BoxFuture};
use kithara_stream::dl::FetchCmd;
use tokio_util::sync::CancellationToken;

/// `Stream<Item = FetchCmd>` that never yields a command and completes
/// only when its cancel token fires.
pub struct NoopCmdStream {
    cancel_fut: BoxFuture<'static, ()>,
}

impl NoopCmdStream {
    /// Build a noop stream tied to a cancel token.
    #[must_use]
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel_fut: Box::pin(async move { cancel.cancelled().await }),
        }
    }
}

impl Stream for NoopCmdStream {
    type Item = FetchCmd;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<FetchCmd>> {
        match self.cancel_fut.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
