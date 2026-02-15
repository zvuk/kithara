#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_net::NetError;
use kithara_storage::{ResourceExt, ResourceStatus, StorageError};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Error type for the generic writer (fetch loop).
///
/// Generic over the source error type `E` (defaults to `NetError` for network streams).
#[derive(Debug, Error)]
pub enum WriterError<E = NetError>
where
    E: std::error::Error + 'static,
{
    #[error("source stream error: {0}")]
    SourceStream(#[source] E),

    #[error("storage write error: {0}")]
    SinkWrite(#[source] StorageError),

    #[error("offset overflowed u64")]
    OffsetOverflow,
}

/// Item yielded by Writer stream.
#[derive(Debug, Clone)]
pub enum WriterItem {
    /// A chunk was written successfully.
    ChunkWritten { offset: u64, len: usize },
    /// Stream ended at this byte offset.
    ///
    /// Resource is NOT automatically committed.
    /// The caller (Downloader) must decide whether to commit based on context
    /// (e.g., comparing `total_bytes` with expected `Content-Length`).
    StreamEnded { total_bytes: u64 },
}

/// Generic writer: any byte stream -> `write_at` -> commit/fail.
///
/// Implements `Stream` trait. Each poll writes a chunk and yields `WriterItem`.
///
/// Type parameter `E` is the error type from the source stream (defaults to `NetError`).
///
/// ## Behavior
///
/// - On any error, calls `res.fail(...)` to unblock readers, then yields error.
/// - On cancellation, stream ends without failing resource (partial data may remain readable).
/// - On success, yields `StreamEnded` with final offset. Does NOT commit.
///
/// ## Examples
///
/// ### Network download (default)
/// ```no_run
/// # use kithara_stream::Writer;
/// # use kithara_net::{Net, NetError};
/// # use kithara_storage::StorageResource;
/// # use tokio_util::sync::CancellationToken;
/// # use url::Url;
/// # async fn example(net: impl Net, res: StorageResource, cancel: CancellationToken) {
/// let url = Url::parse("https://example.com/file.mp3").unwrap();
/// let stream = net.stream(url, None).await.unwrap();
/// let writer: Writer<NetError> = Writer::new(stream, res, cancel);
/// # }
/// ```
///
/// ### Custom source (microphone, file, etc.)
/// ```no_run
/// # use kithara_stream::Writer;
/// # use bytes::Bytes;
/// # use futures::stream;
/// # use kithara_storage::StorageResource;
/// # use tokio_util::sync::CancellationToken;
/// # #[derive(Debug, thiserror::Error)]
/// # #[error("mic error")]
/// # struct MicError;
/// # async fn example(res: StorageResource, cancel: CancellationToken) {
/// // Custom stream from any source
/// let mic_stream = stream::iter(vec![
///     Ok::<Bytes, MicError>(Bytes::from("audio data")),
/// ]);
///
/// let writer: Writer<MicError> = Writer::new(mic_stream, res, cancel);
/// # }
/// ```
pub struct Writer<E = NetError>
where
    E: std::error::Error + 'static,
{
    #[cfg(not(target_arch = "wasm32"))]
    inner: Pin<Box<dyn Stream<Item = Result<WriterItem, WriterError<E>>> + Send>>,
    #[cfg(target_arch = "wasm32")]
    inner: Pin<Box<dyn Stream<Item = Result<WriterItem, WriterError<E>>>>>,
}

/// Shared implementation: the `async_stream` body is target-independent.
///
/// Auto-trait leaking means the concrete type IS Send when S + R are Send,
/// allowing coercion to `dyn Stream + Send` on native.
fn create_writer_stream<S, R, E>(
    mut stream: S,
    res: R,
    cancel: CancellationToken,
    start_offset: u64,
) -> impl Stream<Item = Result<WriterItem, WriterError<E>>>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    R: ResourceExt + Clone + std::fmt::Debug + 'static,
    E: std::error::Error + 'static,
{
    async_stream::stream! {
        let mut offset: u64 = start_offset;
        let mut first_chunk = true;

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!(offset, "writer cancelled");
                    return;
                }

                next = stream.next() => {
                    let Some(next) = next else {
                        // Stream ended — do NOT commit.
                        // Caller decides whether to commit based on context.
                        debug!(offset, "writer stream ended");
                        yield Ok(WriterItem::StreamEnded { total_bytes: offset });
                        return;
                    };

                    let bytes = match next {
                        Ok(b) => b,
                        Err(e) => {
                            res.fail(e.to_string());
                            yield Err(WriterError::SourceStream(e));
                            return;
                        }
                    };

                    if bytes.is_empty() {
                        warn!(offset, "writer received empty net chunk");
                        continue;
                    }

                    if let Err(e) = res.write_at(offset, &bytes) {
                        // Don't mark as failed if another concurrent writer
                        // already committed the resource — the data is valid.
                        if !matches!(res.status(), ResourceStatus::Committed { .. }) {
                            res.fail(e.to_string());
                        }
                        yield Err(WriterError::SinkWrite(e));
                        return;
                    }

                    let chunk_len = bytes.len();
                    let start = offset;
                    if let Some(new_offset) = offset.checked_add(chunk_len as u64) {
                        offset = new_offset;
                    } else {
                        res.fail("offset overflow".to_string());
                        yield Err(WriterError::OffsetOverflow);
                        return;
                    }

                    if first_chunk {
                        debug!(offset, "writer first chunk written");
                        first_chunk = false;
                    }

                    yield Ok(WriterItem::ChunkWritten {
                        offset: start,
                        len: chunk_len,
                    });
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<E> Writer<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    /// Create a new writer from any byte stream source.
    ///
    /// Accepts any `Stream<Item = Result<Bytes, E>>` where `E` is the error type.
    /// This allows writing from network, file, microphone, or any other byte source.
    ///
    /// The caller is responsible for opening the source stream first.
    /// This allows checking cache status before starting the write.
    pub fn new<S, R>(stream: S, res: R, cancel: CancellationToken) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
        R: ResourceExt + Clone + Send + Sync + std::fmt::Debug + 'static,
    {
        Self::with_offset(stream, res, cancel, 0)
    }

    /// Create a writer starting at a specific byte offset.
    ///
    /// Use this for HTTP Range requests where the stream starts at a non-zero offset.
    pub fn with_offset<S, R>(
        stream: S,
        res: R,
        cancel: CancellationToken,
        start_offset: u64,
    ) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
        R: ResourceExt + Clone + Send + Sync + std::fmt::Debug + 'static,
    {
        let inner = Box::pin(create_writer_stream(stream, res, cancel, start_offset));
        Self { inner }
    }
}

#[cfg(target_arch = "wasm32")]
impl<E> Writer<E>
where
    E: std::error::Error + 'static,
{
    pub fn new<S, R>(stream: S, res: R, cancel: CancellationToken) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
        R: ResourceExt + Clone + std::fmt::Debug + 'static,
    {
        Self::with_offset(stream, res, cancel, 0)
    }

    pub fn with_offset<S, R>(
        stream: S,
        res: R,
        cancel: CancellationToken,
        start_offset: u64,
    ) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
        R: ResourceExt + Clone + std::fmt::Debug + 'static,
    {
        let inner = Box::pin(create_writer_stream(stream, res, cancel, start_offset));
        Self { inner }
    }
}

impl<E> Stream for Writer<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    type Item = Result<WriterItem, WriterError<E>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
