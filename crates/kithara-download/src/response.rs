use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
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
#[derive(derive_more::Debug)]
pub struct FetchResponse {
    /// Body as an async byte stream.
    #[debug(skip)]
    pub body: BodyStream,
    /// HTTP response headers.
    pub headers: Headers,
}

/// Async byte stream with cancel + timeout.
///
/// Wraps the raw HTTP body stream. Consumer pulls chunks at own pace,
/// providing natural backpressure. I/O happens on the consumer's task,
/// not on the downloader's worker threads.
#[derive(derive_more::Debug)]
pub struct BodyStream {
    #[debug(skip)]
    inner: InnerStream,
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
        let mut buf = BytesMut::new();
        while let Some(chunk) = self.next().await {
            buf.extend_from_slice(&chunk?);
        }
        Ok(buf.freeze())
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
    /// Received bytes past `limit` end the stream with
    /// [`NetError::TooLarge`], so a body without a declared length is held to
    /// the same bound as one with it.
    pub(super) fn wrap_http(
        byte_stream: ByteStream,
        cancel: CancelGroup,
        limit: Option<u64>,
    ) -> Self {
        Self {
            inner: wrap_with_cancel(byte_stream, cancel, limit),
        }
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
    limit: Option<u64>,
    received: u64,
    done: bool,
}

/// Wrap a [`ByteStream`] with per-chunk cancellation and the request's byte
/// limit. The idle/stall timeout (and its retry/resume) is owned by the net
/// crate's resilient body one layer down, so there is no second idle timer
/// here — only the per-fetch cancel races the body.
fn wrap_with_cancel(
    byte_stream: ByteStream,
    cancel: CancelGroup,
    limit: Option<u64>,
) -> InnerStream {
    Box::pin(stream::unfold(
        WrapState {
            cancel,
            limit,
            stream: byte_stream,
            received: 0,
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
            let item = match chunk? {
                Ok(bytes) => {
                    state.received += bytes.len() as u64;
                    match state.limit {
                        Some(limit) if state.received > limit => {
                            state.done = true;
                            Err(NetError::TooLarge {
                                bytes: state.received,
                                limit,
                            })
                        }
                        _ => Ok(bytes),
                    }
                }
                Err(error) => Err(error),
            };
            Some((item, state))
        },
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures::stream::iter as stream_iter;
    use kithara_test_utils::kithara;

    use super::BodyStream;

    fn test_body_stream(chunks: Vec<&'static [u8]>) -> BodyStream {
        let stream = stream_iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))));
        BodyStream {
            inner: Box::pin(stream),
        }
    }

    #[kithara::test(tokio)]
    async fn body_stream_collect_accumulates_bytes() {
        let body = test_body_stream(vec![b"hello", b" ", b"world"]);
        let result = body.collect().await.expect("collect should succeed");
        assert_eq!(result.as_ref(), b"hello world");
    }

    #[kithara::test(tokio)]
    async fn body_stream_write_all_delegates_to_consumer() {
        let body = test_body_stream(vec![b"abc", b"def"]);
        let mut buf = Vec::new();
        let total = body
            .write_all(|chunk| {
                buf.extend_from_slice(chunk);
                Ok(())
            })
            .await
            .expect("write_all should succeed");

        assert_eq!(total, 6);
        assert_eq!(buf, b"abcdef");
    }

    #[kithara::test(tokio)]
    async fn body_stream_empty_collects_to_empty() {
        let body = test_body_stream(vec![]);
        let result = body.collect().await.expect("collect empty should succeed");
        assert!(result.is_empty());
    }
}
