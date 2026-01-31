#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use kithara_net::NetError;
use kithara_storage::{ResourceExt, StorageError};
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

/// Type alias for network-based writer (most common case).
pub type NetWriter = Writer<NetError>;

/// Item yielded by Writer stream.
#[derive(Debug, Clone)]
pub enum WriterItem {
    /// A chunk was written successfully.
    ChunkWritten { offset: u64, len: usize },
    /// Download completed, resource committed.
    Completed { total_bytes: u64 },
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
/// - On success, commits with final length and yields `Completed`.
///
/// ## Examples
///
/// ### Network download (default)
/// ```no_run
/// # use kithara_stream::{Writer, NetWriter};
/// # use kithara_net::Net;
/// # use kithara_storage::StorageResource;
/// # use tokio_util::sync::CancellationToken;
/// # async fn example(net: impl Net, res: StorageResource, cancel: CancellationToken) {
/// let stream = net.stream("https://example.com/file.mp3").await.unwrap();
/// let writer: NetWriter = Writer::new(stream, res, cancel);
/// // NetWriter is alias for Writer<NetError>
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
    inner: Pin<Box<dyn Stream<Item = Result<WriterItem, WriterError<E>>> + Send>>,
}

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
        let inner = Box::pin(Self::create_stream(stream, res, cancel));
        Self { inner }
    }

    fn create_stream<S, R>(
        mut stream: S,
        res: R,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<WriterItem, WriterError<E>>> + Send
    where
        S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
        R: ResourceExt + Clone + Send + Sync + std::fmt::Debug + 'static,
    {
        async_stream::stream! {
            let mut offset: u64 = 0;
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
                            // Stream ended, commit (sync call in async context is fine — it's fast)
                            if let Err(e) = res.commit(Some(offset)) {
                                yield Err(WriterError::SinkWrite(e));
                                return;
                            }
                            yield Ok(WriterItem::Completed { total_bytes: offset });
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
                            res.fail(e.to_string());
                            yield Err(WriterError::SinkWrite(e));
                            return;
                        }

                        let chunk_len = bytes.len();
                        let start = offset;
                        match offset.checked_add(chunk_len as u64) {
                            Some(new_offset) => offset = new_offset,
                            None => {
                                res.fail("offset overflow".to_string());
                                yield Err(WriterError::OffsetOverflow);
                                return;
                            }
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
}

impl<E> Writer<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    /// Run writer to completion, calling `on_chunk` for each written chunk.
    ///
    /// Returns total bytes written on success.
    pub async fn run<F>(mut self, mut on_chunk: F) -> Result<u64, WriterError<E>>
    where
        F: FnMut(u64, usize),
    {
        use futures::StreamExt as _;
        while let Some(result) = self.next().await {
            match result {
                Ok(WriterItem::ChunkWritten { offset, len }) => {
                    on_chunk(offset, len);
                }
                Ok(WriterItem::Completed { total_bytes }) => {
                    return Ok(total_bytes);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
        Ok(0)
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
