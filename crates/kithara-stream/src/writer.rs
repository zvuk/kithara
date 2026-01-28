#![forbid(unsafe_code)]

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt};
use kithara_net::{ByteStream, NetError};
use kithara_storage::{Resource, StorageError, StreamingResourceExt};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Error type for the generic writer (fetch loop).
#[derive(Debug, Error)]
pub enum WriterError {
    #[error("net stream error: {0}")]
    NetStream(NetError),

    #[error("storage write error: {0}")]
    SinkWrite(StorageError),

    #[error("offset overflowed u64")]
    OffsetOverflow,
}

/// Item yielded by Writer stream.
#[derive(Debug, Clone)]
pub enum WriterItem {
    /// A chunk was written successfully.
    ChunkWritten { offset: u64, len: usize },
    /// Download completed, resource committed.
    Completed { total_bytes: u64 },
}

/// Generic writer: `ByteStream` -> `write_at` -> commit/fail.
///
/// Implements `Stream` trait. Each poll writes a chunk and yields `WriterItem`.
///
/// Behavior:
/// - On any error, calls `res.fail(...)` to unblock readers, then yields error.
/// - On cancellation, stream ends without failing resource (partial data may remain readable).
/// - On success, commits with final length and yields `Completed`.
pub struct Writer {
    inner: Pin<Box<dyn Stream<Item = Result<WriterItem, WriterError>> + Send>>,
}

impl Writer {
    /// Create a new writer from an already-opened byte stream.
    ///
    /// The caller is responsible for opening the network stream first.
    /// This allows checking cache status before starting the download.
    pub fn new<R>(stream: ByteStream, res: R, cancel: CancellationToken) -> Self
    where
        R: StreamingResourceExt + Resource + Clone + Send + Sync + 'static,
    {
        let inner = Box::pin(Self::create_stream(stream, Arc::new(res), cancel));
        Self { inner }
    }

    fn create_stream<R>(
        mut stream: ByteStream,
        res: Arc<R>,
        cancel: CancellationToken,
    ) -> impl Stream<Item = Result<WriterItem, WriterError>> + Send
    where
        R: StreamingResourceExt + Resource + Clone + Send + Sync + 'static,
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
                            // Stream ended, commit
                            if let Err(e) = res.commit(Some(offset)).await {
                                yield Err(WriterError::SinkWrite(e));
                                return;
                            }
                            yield Ok(WriterItem::Completed { total_bytes: offset });
                            return;
                        };

                        let bytes = match next {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = res.fail(e.to_string()).await;
                                yield Err(WriterError::NetStream(e));
                                return;
                            }
                        };

                        if bytes.is_empty() {
                            warn!(offset, "writer received empty net chunk");
                            continue;
                        }

                        if let Err(e) = res.write_at(offset, &bytes).await {
                            let _ = res.fail(e.to_string()).await;
                            yield Err(WriterError::SinkWrite(e));
                            return;
                        }

                        let chunk_len = bytes.len();
                        let start = offset;
                        match offset.checked_add(chunk_len as u64) {
                            Some(new_offset) => offset = new_offset,
                            None => {
                                let _ = res.fail("offset overflow").await;
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

impl Stream for Writer {
    type Item = Result<WriterItem, WriterError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}
