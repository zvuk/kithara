#![forbid(unsafe_code)]

use std::sync::Arc;

use futures::{Stream, StreamExt};
use kithara_net::{Headers, Net, NetError};
use kithara_storage::{Resource, StorageError, StreamingResourceExt, WaitOutcome};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;

use crate::{StreamError, StreamMsg};

/// Error type for the generic writer (fetch loop).
#[derive(Debug, Error)]
pub enum WriterError {
    #[error("net open error: {0}")]
    NetOpen(NetError),

    #[error("net stream error: {0}")]
    NetStream(NetError),

    #[error("storage write error: {0}")]
    SinkWrite(StorageError),

    #[error("offset overflowed u64")]
    OffsetOverflow,
}

/// Error type for the generic reader (feed loop).
#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("wait_range error: {0}")]
    Wait(StorageError),

    #[error("read_at error: {0}")]
    Read(StorageError),

    #[error("empty read after Ready (offset={offset}, len={len})")]
    EmptyAfterReady { offset: u64, len: usize },
}

/// Generic writer: net stream -> `write_at` -> commit/fail.
///
/// Behavior:
/// - On any error, calls `res.fail(...)` to unblock readers, then returns error.
/// - On cancellation, returns Ok(()) without failing resource (partial data may remain readable).
/// - On success, commits with final length.
///
/// This is intentionally minimal and storage-agnostic.
pub struct Writer<N, R, Ev>
where
    N: Net + Send + Sync + 'static,
    R: StreamingResourceExt + Resource + Clone + Send + Sync + 'static,
    Ev: Send + 'static,
{
    net: N,
    url: Url,
    headers: Option<Headers>,
    res: Arc<R>,
    cancel: CancellationToken,
    on_event: Option<Arc<dyn Fn(StreamMsg<(), Ev>) + Send + Sync>>,
    map_event: Option<Arc<dyn Fn(u64, usize) -> Ev + Send + Sync>>,
}

impl<N, R, Ev> Writer<N, R, Ev>
where
    N: Net + Send + Sync + 'static,
    R: StreamingResourceExt + Resource + Clone + Send + Sync + 'static,
    Ev: Send + 'static,
{
    pub fn new(
        net: N,
        url: Url,
        headers: Option<Headers>,
        res: R,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            net,
            url,
            headers,
            res: Arc::new(res),
            cancel,
            on_event: None,
            map_event: None,
        }
    }

    /// Attach an event callback invoked after each successful chunk write.
    /// `map` builds a user-defined event value; `sink` receives `StreamMsg::Event(event)`.
    pub fn with_event<M, S>(mut self, map: M, sink: S) -> Self
    where
        M: Fn(u64, usize) -> Ev + Send + Sync + 'static,
        S: Fn(StreamMsg<(), Ev>) + Send + Sync + 'static,
    {
        self.map_event = Some(Arc::new(map));
        self.on_event = Some(Arc::new(sink));
        self
    }

    pub async fn run(&self) -> Result<(), WriterError> {
        let mut stream = self
            .net
            .stream(self.url.clone(), self.headers.clone())
            .await
            .map_err(WriterError::NetOpen)?;

        let mut offset: u64 = 0;
        let mut first_chunk = true;

        loop {
            tokio::select! {
                () = self.cancel.cancelled() => {
                    debug!(offset, "writer cancelled");
                    // Do not fail resource on cancel; partial data may be useful.
                    return Ok(());
                }

                next = stream.next() => {
                    let Some(next) = next else { break; };

                    let bytes = next.map_err(WriterError::NetStream)?;
                    if bytes.is_empty() {
                        warn!(offset, "writer received empty net chunk");
                        // treat as benign: skip and continue
                        continue;
                    }

                    self.res
                        .write_at(offset, &bytes)
                        .await
                        .map_err(WriterError::SinkWrite)?;

                    let chunk_len = bytes.len();
                    let start = offset;
                    offset = offset
                        .checked_add(chunk_len as u64)
                        .ok_or(WriterError::OffsetOverflow)?;

                    if let (Some(build), Some(sink)) = (&self.map_event, &self.on_event) {
                        let ev = build(start, chunk_len);
                        sink(StreamMsg::Event(ev));
                    }

                    if first_chunk {
                        debug!(offset, "writer first chunk written");
                        first_chunk = false;
                    }
                }
            }
        }

        self.res
            .commit(Some(offset))
            .await
            .map_err(WriterError::SinkWrite)?;
        Ok(())
    }

    /// Helper that materializes an error into the resource and returns it.
    pub async fn run_with_fail(&self) -> Result<(), WriterError> {
        match self.run().await {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                let _ = self.res.fail(msg).await;
                Err(e)
            }
        }
    }
}

/// Generic reader: `wait_range` -> `read_at` -> yield bytes.
///
/// Emits only `StreamMsg::Data(Bytes)`; control/events are the caller's responsibility.
pub struct Reader<R>
where
    R: StreamingResourceExt + Send + Sync + 'static,
{
    res: R,
    start_pos: u64,
    chunk_size: usize,
}

impl<R> Reader<R>
where
    R: StreamingResourceExt + Send + Sync + 'static,
{
    pub fn new(res: R, start_pos: u64, chunk_size: usize) -> Self {
        Self {
            res,
            start_pos,
            chunk_size,
        }
    }

    pub fn into_stream<Ev>(
        self,
    ) -> impl Stream<Item = Result<StreamMsg<(), Ev>, StreamError<ReaderError>>> + Send + 'static
    where
        Ev: Send + 'static,
    {
        let chunk = self.chunk_size;
        let mut pos = self.start_pos;
        async_stream::stream! {
            loop {
                let end = pos.saturating_add(chunk as u64);

                let outcome = self.res.wait_range(pos..end).await;
                let Ok(WaitOutcome::Ready) = outcome else {
                    if let Err(e) = outcome {
                        yield Err(StreamError::Source(ReaderError::Wait(e)));
                    }
                    return;
                };

                let bytes = self
                    .res
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
        }
    }
}
