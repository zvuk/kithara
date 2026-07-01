use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use kithara_net::{ByteStream, Headers, NetError};
use kithara_platform::{CancelGroup, tokio};

/// Boxed inner stream used inside [`BodyStream`].
///
/// On native: requires `Send` (multi-threaded tokio runtime).
/// On wasm32: no `Send` bound (JsValue-backed streams are `!Send` and
/// the browser tokio runtime is single-threaded — `Send` is vacuous).
#[cfg(not(target_arch = "wasm32"))]
type InnerStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>> + Send>>;
#[cfg(target_arch = "wasm32")]
type InnerStream = Pin<Box<dyn Stream<Item = Result<Bytes, NetError>>>>;

/// Response from a fetch — headers available immediately, body as
/// async stream.
pub struct FetchResponse {
    /// Body as an async byte stream.
    pub body: BodyStream,
    /// HTTP response headers.
    pub headers: Headers,
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
    inner: InnerStream,
}

impl std::fmt::Debug for BodyStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyStream").finish_non_exhaustive()
    }
}

/// Bytes-backed body — `Send` on every target. Used to ferry a fully buffered
/// channel-path response across the wasm worker boundary (the raw HTTP stream
/// is `!Send` on wasm; collecting it on the download worker and re-wrapping the
/// bytes keeps the boundary clean).
impl From<Bytes> for BodyStream {
    fn from(bytes: Bytes) -> Self {
        Self {
            inner: Box::pin(stream::once(async move { Ok(bytes) })),
        }
    }
}

impl BodyStream {
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

    /// Empty body (for HEAD responses).
    pub(super) fn empty() -> Self {
        Self {
            inner: Box::pin(stream::empty()),
        }
    }

    /// Wrap an HTTP [`ByteStream`] with per-chunk cancellation.
    ///
    /// The idle/stall timeout and its retry/resume live one layer down in
    /// the net crate's resilient body (`HttpClient` wraps every streaming
    /// fetch), which is the single owner of stall detection — so this
    /// wrapper only races the body against the per-fetch cancel and never
    /// imposes a second, conflicting idle timer.
    pub(super) fn wrap_http(byte_stream: ByteStream, cancel: CancelGroup) -> Self {
        Self {
            inner: wrap_with_cancel(byte_stream, cancel),
        }
    }

    /// Wrap a raw stream (for testing or non-HTTP sources).
    #[must_use]
    pub fn wrap_raw(inner: InnerStream) -> Self {
        Self { inner }
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
            writer(data.as_ref()).map_err(|e| NetError::Decode(e.to_string()))?;
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

/// State for the cancel body stream wrapper.
struct WrapState {
    stream: ByteStream,
    cancel: CancelGroup,
    done: bool,
}

/// Wrap a [`ByteStream`] with per-chunk cancellation. The idle/stall
/// timeout (and its retry/resume) is owned by the net crate's resilient
/// body one layer down, so there is no second idle timer here — only the
/// per-fetch cancel races the body.
fn wrap_with_cancel(byte_stream: ByteStream, cancel: CancelGroup) -> InnerStream {
    Box::pin(stream::unfold(
        WrapState {
            cancel,
            stream: byte_stream,
            done: false,
        },
        |mut state| async {
            if state.done {
                return None;
            }
            let chunk = tokio::select! {
                biased;
                () = state.cancel.cancelled() => {
                    state.done = true;
                    return Some((Err(NetError::Cancelled), state));
                },
                c = state.stream.next() => c,
            };
            chunk.map(|item| (item, state))
        },
    ))
}
