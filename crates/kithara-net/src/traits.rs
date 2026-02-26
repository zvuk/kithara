use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use kithara_platform::{MaybeSend, MaybeSync};
use tokio_util::sync::CancellationToken;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-utils")))]
use unimock::unimock;
use url::Url;

use crate::{
    error::NetError,
    retry::{DefaultRetryPolicy, RetryNet},
    timeout::TimeoutNet,
    types::{Headers, RangeSpec, RetryPolicy},
};

/// Inner stream type used inside [`ByteStream`].
///
/// On native: requires `Send` (multi-threaded tokio runtime).
/// On wasm32: no `Send` bound (JsValue-based streams are `!Send`).
#[cfg(not(target_arch = "wasm32"))]
type RawByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;
#[cfg(target_arch = "wasm32")]
type RawByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>>>>;

/// HTTP byte stream with response headers.
///
/// Wraps a streaming body together with the HTTP response headers so that
/// callers can inspect metadata (e.g. `Content-Length`) without an extra
/// HEAD request.
///
/// Implements [`Stream`] by delegating to the inner body, so it can be
/// passed directly to any consumer that expects a byte stream.
pub struct ByteStream {
    /// Response headers from the HTTP request that produced this stream.
    pub headers: Headers,
    inner: RawByteStream,
}

impl ByteStream {
    /// Create a new `ByteStream` from response headers and a raw body stream.
    #[must_use]
    pub fn new(headers: Headers, inner: RawByteStream) -> Self {
        Self { headers, inner }
    }

    /// Create a `ByteStream` with empty headers (for tests or non-HTTP sources).
    #[must_use]
    pub fn without_headers(inner: RawByteStream) -> Self {
        Self {
            headers: Headers::new(),
            inner,
        }
    }

    /// Consume the wrapper, returning just the raw byte stream.
    #[must_use]
    pub fn into_inner(self) -> RawByteStream {
        self.inner
    }
}

impl Stream for ByteStream {
    type Item = Result<Bytes, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // ByteStream is Unpin (Headers is Unpin, Pin<Box<_>> is Unpin),
        // so we can safely access fields through get_mut().
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

/// HTTP networking trait.
///
/// Single definition for both native and wasm32 targets.
/// On native: `MaybeSend` = `Send`, `MaybeSync` = `Sync`, futures are `Send`.
/// On wasm32: `MaybeSend`/`MaybeSync` are blanket-implemented (no-op), futures are `!Send`.
#[cfg_attr(
    all(not(target_arch = "wasm32"), any(test, feature = "test-utils")),
    unimock(api = NetMock)
)]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait Net: MaybeSend + MaybeSync {
    /// Get all bytes from a URL
    async fn get_bytes(&self, url: Url, headers: Option<Headers>) -> Result<Bytes, NetError>;

    /// Stream bytes from a URL
    async fn stream(&self, url: Url, headers: Option<Headers>) -> Result<ByteStream, NetError>;

    /// Get a range of bytes from a URL
    async fn get_range(
        &self,
        url: Url,
        range: RangeSpec,
        headers: Option<Headers>,
    ) -> Result<ByteStream, NetError>;

    /// Perform a HEAD request.
    ///
    /// This is intended for lightweight metadata probes (e.g. `Content-Length`,
    /// `Accept-Ranges`, `Content-Type`). Implementations should return response headers.
    async fn head(&self, url: Url, headers: Option<Headers>) -> Result<Headers, NetError>;
}

pub trait NetExt: Net + Sized {
    /// Add timeout layer
    fn with_timeout(self, timeout: Duration) -> TimeoutNet<Self> {
        TimeoutNet::new(self, timeout)
    }

    /// Add retry layer
    fn with_retry(
        self,
        policy: RetryPolicy,
        cancel: CancellationToken,
    ) -> RetryNet<Self, DefaultRetryPolicy> {
        RetryNet::new(self, DefaultRetryPolicy::new(policy), cancel)
    }
}

impl<T: Net> NetExt for T {}
