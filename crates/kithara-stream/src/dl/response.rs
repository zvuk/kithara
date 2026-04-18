//! Fetch response types for the channel-based downloader API.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use kithara_net::{ByteStream, Headers, NetError};
use kithara_platform::{
    CancelGroup,
    time::{Duration, sleep},
    tokio,
};

/// Response from a fetch — headers available immediately, body as
/// async stream.
pub struct FetchResponse {
    /// HTTP response headers.
    pub headers: Headers,
    /// Body as an async byte stream.
    pub body: BodyStream,
}

impl std::fmt::Debug for FetchResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchResponse")
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Async byte stream with cancel + timeout.
///
/// Wraps the raw HTTP body stream. Consumer pulls chunks at own pace,
/// providing natural backpressure. I/O happens on the consumer's task,
/// not on the downloader's worker threads.
pub struct BodyStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>,
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStream").finish_non_exhaustive()
    }
}

impl BodyStream {
    /// Empty body (for HEAD responses).
    pub(super) fn empty() -> Self {
        Self {
            inner: Box::pin(stream::empty()),
        }
    }

    /// Wrap a raw stream (for testing or non-HTTP sources).
    #[must_use]
    pub fn from_raw(inner: Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>) -> Self {
        Self { inner }
    }

    /// Wrap an HTTP [`ByteStream`] with per-chunk cancel + timeout.
    pub(super) fn from_http(
        byte_stream: ByteStream,
        cancel: CancelGroup,
        chunk_timeout: Duration,
    ) -> Self {
        Self {
            inner: wrap_with_cancel(byte_stream, cancel, chunk_timeout),
        }
    }

    /// Collect entire body into bytes.
    ///
    /// Use for small control-plane responses (playlists, DRM keys).
    ///
    /// # Errors
    /// Returns an error when the underlying stream yields a network
    /// error or the cancel token fires.
    pub async fn collect(mut self) -> Result<Bytes, NetError> {
        let mut buf = Vec::new();
        while let Some(chunk) = self.next().await {
            buf.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(buf))
    }

    /// Stream chunks through a writer, return total bytes written.
    ///
    /// The writer runs on the consumer's task — not on the downloader's
    /// worker threads.
    ///
    /// # Errors
    /// Returns an error when the stream yields a network error, the
    /// writer returns an I/O error, or the cancel token fires.
    pub async fn write_all<W>(mut self, mut writer: W) -> Result<u64, NetError>
    where
        W: FnMut(&[u8]) -> std::io::Result<()>,
    {
        let mut total: u64 = 0;
        while let Some(chunk) = self.next().await {
            let data = chunk?;
            writer(data.as_ref()).map_err(|e| NetError::Http(e.to_string()))?;
            total += data.len() as u64;
        }
        Ok(total)
    }
}

impl Stream for BodyStream {
    type Item = Result<Bytes, NetError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

/// State for the cancel+timeout body stream wrapper.
struct WrapState {
    stream: ByteStream,
    cancel: CancelGroup,
    timeout: Duration,
    done: bool,
}

/// Wrap a [`ByteStream`] with per-chunk cancellation and idle timeout.
fn wrap_with_cancel(
    byte_stream: ByteStream,
    cancel: CancelGroup,
    chunk_timeout: Duration,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>> {
    Box::pin(stream::unfold(
        WrapState {
            stream: byte_stream,
            cancel,
            timeout: chunk_timeout,
            done: false,
        },
        |mut state| async {
            if state.done {
                return None;
            }
            let chunk = tokio::select! {
                () = state.cancel.cancelled() => {
                    state.done = true;
                    return Some((Err(NetError::Cancelled), state));
                }
                c = state.stream.next() => c,
                () = sleep(state.timeout) => None,
            };
            chunk.map(|item| (item, state))
        },
    ))
}
