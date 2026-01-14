#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use async_trait::async_trait;
use bytes::Bytes;
use kithara_storage::WaitOutcome;
use tracing::trace;

use crate::error::{StreamError, StreamResult};

/// Async random-access source contract.
///
/// This is intentionally minimal and does **not** depend on any storage implementation.
/// `kithara-file` / `kithara-hls` should implement this for their internal byte providers.
///
/// Normative:
/// - `wait_range(range)` must block until the entire `range` is readable, OR return `Eof`
///   if `range.start` is at/after EOF, OR return error if the source fails/cancels.
/// - `read_at(offset, len)` must return up to `len` bytes without implicitly waiting.
///   If the caller needs blocking semantics it must call `wait_range` first.
/// - When `offset` is at/after EOF (and EOF is known), `read_at` must return `Bytes::new()`.
#[async_trait]
pub trait Source: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error>
    where
        Self: Send + Sync;
    async fn read_at(&self, offset: u64, len: usize) -> StreamResult<Bytes, Self::Error>
    where
        Self: Send + Sync;

    /// Return known total length if available.
    ///
    /// - `Some(len)` enables `SeekFrom::End(..)` and validation for seeking past EOF.
    /// - `None` means length is unknown (still seekable via absolute positions, but
    ///   seeking relative to end is unsupported).
    fn len(&self) -> Option<u64>;

    /// Return true if length is known to be zero.
    ///
    /// Default implementation derives from `len()`.
    fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }
}

enum WorkerReq<S>
where
    S: Source,
{
    WaitAndReadAt {
        offset: u64,
        len: usize,
        reply: tokio::sync::oneshot::Sender<StreamResult<Bytes, S::Error>>,
    },
}

/// Sync `Read + Seek` adapter over a [`Source`].
///
/// This is designed specifically to satisfy consumers like `rodio::Decoder`.
///
/// Blocking behavior:
/// - `read()` blocks waiting for an async worker running on a Tokio runtime.
/// - Do **not** call this from within a Tokio async task (it will block the executor thread).
///   Use it from a dedicated blocking thread (e.g. `tokio::task::spawn_blocking` or `std::thread`).
pub struct SyncReader<S>
where
    S: Source,
{
    source: Arc<S>,
    worker_tx: tokio::sync::mpsc::UnboundedSender<WorkerReq<S>>,
    pos: u64,
}

impl<S> SyncReader<S>
where
    S: Source,
{
    /// Create a new reader bound to the current Tokio runtime.
    ///
    /// This spawns an async worker on the current Tokio runtime and communicates with it via
    /// channels. This avoids calling `Handle::block_on` from arbitrary threads, which can
    /// deadlock with some executor / storage combinations.
    ///
    /// This must be called from within a Tokio runtime context (e.g. in an async fn before
    /// spawning the blocking consumer).
    pub fn new(source: Arc<S>) -> Self {
        trace!("kithara-stream::io Reader::new (spawning async worker)");
        let len = source.len();
        trace!(len, "kithara-stream::io Reader created");

        let (worker_tx, mut worker_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerReq<S>>();
        let src = source.clone();

        tokio::spawn(async move {
            trace!("kithara-stream::io Reader worker started");
            while let Some(req) = worker_rx.recv().await {
                match req {
                    WorkerReq::WaitAndReadAt { offset, len, reply } => {
                        let range = offset..offset.saturating_add(len as u64);
                        let wait_result = src.wait_range(range).await;

                        let out = match wait_result {
                            Ok(WaitOutcome::Ready) => src.read_at(offset, len).await,
                            Ok(WaitOutcome::Eof) => Ok(Bytes::new()),
                            Err(e) => Err(e),
                        };

                        let _ = reply.send(out);
                    }
                }
            }
            trace!("kithara-stream::io Reader worker stopped (channel closed)");
        });

        Self {
            source,
            worker_tx,
            pos: 0,
        }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_source(self) -> Arc<S> {
        self.source
    }
}

impl<S> Read for SyncReader<S>
where
    S: Source,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let offset = self.pos;
        let len = buf.len();

        trace!(
            offset,
            requested_len = len,
            buffer_size = buf.len(),
            "SyncReader::read START"
        );
        let read_start = std::time::Instant::now();

        let (tx, rx) = tokio::sync::oneshot::channel();
        let send_start = std::time::Instant::now();
        self.worker_tx
            .send(WorkerReq::WaitAndReadAt {
                offset,
                len,
                reply: tx,
            })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Reader worker stopped")
            })?;
        let send_duration = send_start.elapsed();
        trace!(
            offset,
            send_duration_ms = send_duration.as_millis(),
            "SyncReader::send to worker DONE"
        );

        let recv_start = std::time::Instant::now();
        let bytes = rx
            .blocking_recv()
            .map_err(|_| {
                tracing::error!(
                    offset,
                    len,
                    "Reader::read wait_and_read_at worker reply channel closed"
                );
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Reader worker stopped")
            })?
            .map_err(|e: StreamError<S::Error>| {
                tracing::error!(
                    offset,
                    len,
                    err = %e,
                    "Reader::read wait_and_read_at returned error"
                );
                std::io::Error::other(e.to_string())
            })?;
        let recv_duration = recv_start.elapsed();
        trace!(
            offset,
            recv_duration_ms = recv_duration.as_millis(),
            bytes_received = bytes.len(),
            "SyncReader::blocking_recv DONE"
        );

        if bytes.is_empty() {
            // Empty bytes means EOF (either from WaitOutcome::Eof or read_at returning empty at EOF)
            let total_duration = read_start.elapsed();
            trace!(
                offset,
                total_duration_ms = total_duration.as_millis(),
                "SyncReader::read EOF"
            );
            return Ok(0);
        }

        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        self.pos = self.pos.saturating_add(n as u64);
        let total_duration = read_start.elapsed();
        trace!(
            offset,
            total_duration_ms = total_duration.as_millis(),
            requested = len,
            read = n,
            new_pos = self.pos,
            "SyncReader::read DONE"
        );
        Ok(n)
    }
}

impl<S> Seek for SyncReader<S>
where
    S: Source,
{
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        trace!(cur = self.pos, ?pos, "Reader::seek enter");

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    trace!("Reader::seek from end requested but source len is unknown");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        StreamError::<S::Error>::UnknownLength.to_string(),
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            trace!(new_pos, "Reader::seek invalid (negative)");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        let new_pos_u64 = new_pos as u64;

        // If we know length, disallow seeking past EOF to match typical `Read+Seek` expectations
        // for decoders that probe file structure.
        if let Some(len) = self.source.len()
            && new_pos_u64 > len
        {
            trace!(new_pos_u64, len, "Reader::seek invalid (past EOF)");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        self.pos = new_pos_u64;
        Ok(self.pos)
    }
}
