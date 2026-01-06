#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use bytes::Bytes;
use futures::{Stream, StreamExt, future::BoxFuture};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{StreamError, StreamMsg};

/// Outcome of waiting for a byte range.
///
/// This mirrors the semantics of `kithara-storage::WaitOutcome` but lives here to keep the
/// fetch/reader layer generic over the underlying storage implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Ready,
    Eof,
}

/// Abstract network adapter.
///
/// Higher-level crates (`kithara-file`, `kithara-hls`) implement this for their HTTP client.
///
/// Contract:
/// - `stream(req)` opens a byte stream (optionally range-aware).
pub trait Net: Send + Sync + 'static {
    type Request: Send + Sync + Clone + 'static;
    type Error: std::error::Error + Send + Sync + 'static;
    type ByteStream: Stream<Item = Result<Bytes, Self::Error>> + Send + Unpin + 'static;

    fn stream(
        &self,
        req: Self::Request,
    ) -> BoxFuture<'static, Result<Self::ByteStream, Self::Error>>;
}

/// Abstract sink that supports random-access writes + lifecycle.
pub trait WriteSink: Send + Sync + Clone + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn write_at<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
    ) -> BoxFuture<'a, Result<(), Self::Error>>;

    fn commit<'a>(&'a self, final_len: Option<u64>) -> BoxFuture<'a, Result<(), Self::Error>>;

    fn fail<'a>(&'a self, msg: String) -> BoxFuture<'a, Result<(), Self::Error>>;
}

/// Abstract source that supports waitable random-access reads.
pub trait ReadSource: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn wait_range<'a>(
        &'a self,
        range: Range<u64>,
    ) -> BoxFuture<'a, Result<WaitOutcome, Self::Error>>;

    fn read_at<'a>(&'a self, offset: u64, len: usize) -> BoxFuture<'a, Result<Bytes, Self::Error>>;
}

/// Error type for the generic writer (fetch loop).
#[derive(Debug, Error)]
pub enum WriterError<NE, SE>
where
    NE: std::error::Error + Send + Sync + 'static,
    SE: std::error::Error + Send + Sync + 'static,
{
    #[error("net open error: {0}")]
    NetOpen(NE),

    #[error("net stream error: {0}")]
    NetStream(NE),

    #[error("sink write_at error: {0}")]
    SinkWrite(SE),

    #[error("offset overflowed u64")]
    OffsetOverflow,
}

/// Error type for the generic reader (feed loop).
#[derive(Debug, Error)]
pub enum ReaderError<SE>
where
    SE: std::error::Error + Send + Sync + 'static,
{
    #[error("wait_range error: {0}")]
    Wait(SE),

    #[error("read_at error: {0}")]
    Read(SE),

    #[error("empty read after Ready (offset={offset}, len={len})")]
    EmptyAfterReady { offset: u64, len: usize },
}

/// Generic writer: net stream -> write_at -> commit/fail.
///
/// Behavior:
/// - On any error, calls `sink.fail(...)` to unblock readers, then returns error.
/// - On cancellation, returns Ok(()) without failing sink (partial data may remain readable).
/// - On success, commits with final length.
///
/// This is intentionally minimal and storage-agnostic.
pub struct Writer<N, S>
where
    N: Net,
    S: WriteSink,
{
    net: N,
    req: N::Request,
    sink: Arc<S>,
    cancel: CancellationToken,
}

impl<N, S> Writer<N, S>
where
    N: Net,
    S: WriteSink,
{
    pub fn new(net: N, req: N::Request, sink: S, cancel: CancellationToken) -> Self {
        Self {
            net,
            req,
            sink: Arc::new(sink),
            cancel,
        }
    }

    pub async fn run(&self) -> Result<(), WriterError<N::Error, S::Error>> {
        let mut stream = self
            .net
            .stream(self.req.clone())
            .await
            .map_err(WriterError::NetOpen)?;

        let mut offset: u64 = 0;
        let mut chunks: u64 = 0;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    debug!(offset, "writer cancelled");
                    // Do not fail sink on cancel; partial data may be useful.
                    return Ok(());
                }

                next = stream.next() => {
                    let Some(next): Option<Result<Bytes, N::Error>> = next else { break; };

                    let bytes = next.map_err(WriterError::NetStream)?;
                    if bytes.is_empty() {
                        warn!(offset, "writer received empty net chunk");
                        // treat as benign: skip and continue
                        continue;
                    }

                    self.sink
                        .write_at(offset, &bytes)
                        .await
                        .map_err(WriterError::SinkWrite)?;

                    offset = offset
                        .checked_add(bytes.len() as u64)
                        .ok_or(WriterError::OffsetOverflow)?;

                    chunks = chunks.saturating_add(1);
                    if chunks == 1 {
                        debug!(offset, "writer first chunk written");
                    }
                }
            }
        }

        let _ = self.sink.commit(Some(offset)).await;
        Ok(())
    }

    /// Helper that materializes an error into the sink and returns it.
    pub async fn run_with_fail(&self) -> Result<(), WriterError<N::Error, S::Error>> {
        match self.run().await {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                let _ = self.sink.fail(msg).await;
                Err(e)
            }
        }
    }
}

/// Generic reader: wait_range -> read_at -> yield bytes.
///
/// Emits only `StreamMsg::Data(Bytes)`; control/events are the caller's responsibility.
pub struct Reader<S>
where
    S: ReadSource,
{
    source: S,
    start_pos: u64,
    chunk_size: usize,
}

impl<S> Reader<S>
where
    S: ReadSource,
{
    pub fn new(source: S, start_pos: u64, chunk_size: usize) -> Self {
        Self {
            source,
            start_pos,
            chunk_size,
        }
    }

    pub fn into_stream<Ev>(
        self,
    ) -> impl Stream<Item = Result<StreamMsg<(), Ev>, StreamError<ReaderError<S::Error>>>> + Send + 'static
    where
        Ev: Send + 'static,
    {
        let chunk = self.chunk_size;
        let mut pos = self.start_pos;
        async_stream::stream! {
            loop {
                let end = pos.saturating_add(chunk as u64);

                match self.source.wait_range(pos..end).await {
                    Ok(WaitOutcome::Ready) => {
                        let bytes = self
                            .source
                            .read_at(pos, chunk)
                            .await
                            .map_err(ReaderError::Read)
                            .map_err(StreamError::Source)?;

                        if bytes.is_empty() {
                            yield Err(StreamError::Source(ReaderError::EmptyAfterReady {
                                offset: pos,
                                len: chunk,
                            }));
                            return;
                        }

                        pos = pos.saturating_add(bytes.len() as u64);
                        yield Ok(StreamMsg::Data(bytes));
                    }
                    Ok(WaitOutcome::Eof) => {
                        return;
                    }
                    Err(e) => {
                        yield Err(StreamError::Source(ReaderError::Wait(e)));
                        return;
                    }
                }
            }
        }
    }
}
