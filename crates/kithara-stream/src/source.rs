#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    ops::Range,
    sync::Arc,
};

use async_trait::async_trait;
use kithara_storage::WaitOutcome;
use tokio::sync::mpsc;
use tracing::trace;

use crate::{
    error::{StreamError, StreamResult},
    prefetch::{PrefetchSource, PrefetchWorker, PrefetchedItem},
};

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
/// - When `offset` is at/after EOF (and EOF is known), `read_at` must return `Ok(0)`.
#[async_trait]
pub trait Source: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error>
    where
        Self: Send + Sync;

    /// Read bytes at `offset` into `buf`.
    ///
    /// Returns the number of bytes read. Returns `Ok(0)` for EOF.
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> StreamResult<usize, Self::Error>
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

/// Configuration for `SyncReader`.
#[derive(Debug, Clone)]
pub struct SyncReaderParams {
    /// Size of each prefetch chunk in bytes.
    pub chunk_size: usize,
    /// Number of chunks to prefetch ahead.
    pub prefetch_chunks: usize,
}

impl Default for SyncReaderParams {
    fn default() -> Self {
        Self {
            chunk_size: 64 * 1024, // 64KB
            prefetch_chunks: 4,
        }
    }
}

/// Byte chunk with position information.
#[derive(Debug, Clone)]
pub struct ByteChunk {
    /// Position in source where this chunk starts.
    pub file_pos: u64,
    /// Chunk data.
    pub data: Vec<u8>,
}

/// Seek command for byte source.
#[derive(Debug, Clone, Copy)]
pub struct ByteSeekCmd {
    /// Target position.
    pub pos: u64,
    /// Epoch to synchronize with reader.
    pub epoch: u64,
}

/// Prefetch source adapter for `Source` trait.
///
/// This wraps a `Source` and implements `PrefetchSource` to work with the generic
/// `PrefetchWorker`.
pub struct BytePrefetchSource<S: Source> {
    src: Arc<S>,
    chunk_size: usize,
    read_pos: u64,
    epoch: u64,
}

impl<S: Source> BytePrefetchSource<S> {
    /// Create a new byte prefetch source.
    pub fn new(src: Arc<S>, chunk_size: usize) -> Self {
        Self {
            src,
            chunk_size,
            read_pos: 0,
            epoch: 0,
        }
    }
}

impl<S: Source> PrefetchSource for BytePrefetchSource<S> {
    type Chunk = ByteChunk;
    type Command = ByteSeekCmd;

    async fn fetch_next(&mut self) -> Option<Self::Chunk> {
        // Wait for data
        let range = self.read_pos..self.read_pos.saturating_add(self.chunk_size as u64);

        let wait_result = self.src.wait_range(range).await;

        match wait_result {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => {
                trace!(
                    pos = self.read_pos,
                    epoch = self.epoch,
                    "BytePrefetchSource: EOF"
                );
                return None;
            }
            Err(e) => {
                tracing::error!(err = %e, "BytePrefetchSource: wait_range error");
                return None;
            }
        }

        // Read data
        let mut buf = vec![0u8; self.chunk_size];
        let read_result = self.src.read_at(self.read_pos, &mut buf).await;

        match read_result {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                let chunk = ByteChunk {
                    file_pos: self.read_pos,
                    data: buf,
                };
                trace!(
                    file_pos = self.read_pos,
                    bytes = n,
                    epoch = self.epoch,
                    "BytePrefetchSource: fetched"
                );
                self.read_pos = self.read_pos.saturating_add(n as u64);
                Some(chunk)
            }
            Ok(_) => {
                // n == 0 means EOF
                trace!(pos = self.read_pos, "BytePrefetchSource: EOF (read 0)");
                None
            }
            Err(e) => {
                tracing::error!(err = %e, "BytePrefetchSource: read_at error");
                None
            }
        }
    }

    fn handle_command(&mut self, cmd: Self::Command) -> u64 {
        trace!(
            from = self.read_pos,
            to = cmd.pos,
            epoch = cmd.epoch,
            "BytePrefetchSource: seek"
        );
        self.read_pos = cmd.pos;
        self.epoch = cmd.epoch;
        self.epoch
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn eof_chunk(&self) -> Self::Chunk {
        ByteChunk {
            file_pos: self.read_pos,
            data: Vec::new(),
        }
    }
}

/// Sync `Read + Seek` adapter over a [`Source`].
///
/// This is designed specifically to satisfy consumers like `rodio::Decoder`.
///
/// Non-blocking behavior:
/// - `read()` returns immediately with available data from prefetch buffer.
/// - If no data is available yet, returns 0 (underrun) instead of blocking.
/// - An async worker prefetches data ahead of the read position.
///
/// This design ensures the audio thread is never blocked by I/O operations.
pub struct SyncReader<S>
where
    S: Source,
{
    source: Arc<S>,
    cmd_tx: mpsc::Sender<ByteSeekCmd>,
    data_rx: kanal::Receiver<PrefetchedItem<ByteChunk>>,
    current_chunk: Option<CurrentChunk>,
    pos: u64,
    epoch: u64,
    eof_reached: bool,
}

/// Current chunk being consumed by reader.
struct CurrentChunk {
    file_pos: u64,
    data: Vec<u8>,
    offset: usize,
    epoch: u64,
}

impl<S> SyncReader<S>
where
    S: Source,
{
    /// Create a new reader bound to the current Tokio runtime.
    ///
    /// This spawns an async worker that continuously prefetches data ahead of the
    /// current read position. The worker communicates via channels using non-blocking
    /// `try_recv()` on the reader side.
    ///
    /// This must be called from within a Tokio runtime context.
    pub fn new(source: Arc<S>, params: SyncReaderParams) -> Self {
        let chunk_size = params.chunk_size.max(4096);
        let prefetch_chunks = params.prefetch_chunks.max(2);
        trace!(
            prefetch_chunks,
            chunk_size, "SyncReader::new (spawning prefetch worker)"
        );

        let (cmd_tx, cmd_rx) = mpsc::channel::<ByteSeekCmd>(4);
        let (data_tx, data_rx) = kanal::bounded::<PrefetchedItem<ByteChunk>>(prefetch_chunks);

        let prefetch_source = BytePrefetchSource::new(source.clone(), chunk_size);
        let worker = PrefetchWorker::new(prefetch_source, cmd_rx, data_tx.to_async());
        tokio::spawn(worker.run());

        Self {
            source,
            cmd_tx,
            data_rx,
            current_chunk: None,
            pos: 0,
            epoch: 0,
            eof_reached: false,
        }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_source(self) -> Arc<S> {
        self.source
    }

    /// Get reference to the underlying source.
    pub(crate) fn source_ref(&self) -> &S {
        &self.source
    }

    /// Wait for and consume data from prefetch buffer.
    /// Uses kanal's efficient blocking `recv()` with thread parking.
    fn recv_chunk(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // First, try to read from current chunk
        if let Some(ref mut chunk) = self.current_chunk {
            if chunk.epoch == self.epoch && chunk.file_pos + chunk.offset as u64 == self.pos {
                let remaining = chunk.data.len() - chunk.offset;
                if remaining > 0 {
                    let n = remaining.min(buf.len());
                    buf[..n].copy_from_slice(&chunk.data[chunk.offset..chunk.offset + n]);
                    chunk.offset += n;
                    return Ok(n);
                }
            }
            // Chunk exhausted or epoch/position mismatch, discard it
            self.current_chunk = None;
        }

        // Wait for matching chunk (non-blocking loop with yield)
        loop {
            match self.data_rx.try_recv() {
                Ok(Some(item)) => {
                    if let Some(n) = self.process_item(item, buf) {
                        return Ok(n);
                    }
                    // Item was stale, continue waiting
                }
                Ok(None) => {
                    // No data yet, yield and retry
                    std::thread::yield_now();
                }
                Err(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Worker stopped",
                    ));
                }
            }
        }
    }

    /// Process a received prefetched item, returning Some(bytes) if valid or None if stale.
    fn process_item(&mut self, item: PrefetchedItem<ByteChunk>, buf: &mut [u8]) -> Option<usize> {
        // Skip items from old epochs
        if item.epoch != self.epoch {
            trace!(
                item_epoch = item.epoch,
                reader_epoch = self.epoch,
                "Skipping old epoch item"
            );
            return None;
        }

        // Check EOF marker (empty data with is_eof flag or empty chunk at expected position)
        if item.is_eof || item.data.data.is_empty() {
            self.eof_reached = true;
            return Some(0);
        }

        let chunk = item.data;

        // Check if chunk matches our position
        if chunk.file_pos == self.pos {
            let n = chunk.data.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk.data[..n]);

            if n < chunk.data.len() {
                self.current_chunk = Some(CurrentChunk {
                    file_pos: chunk.file_pos,
                    data: chunk.data,
                    offset: n,
                    epoch: item.epoch,
                });
            }
            return Some(n);
        }

        // Position mismatch - stale chunk
        trace!(
            chunk_pos = chunk.file_pos,
            reader_pos = self.pos,
            "Skipping stale chunk"
        );
        None
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

        if self.eof_reached {
            return Ok(0);
        }

        // Wait for data using kanal's efficient blocking (thread parking, no spin)
        let n = self.recv_chunk(buf)?;
        if n > 0 {
            self.pos = self.pos.saturating_add(n as u64);
            trace!(bytes = n, new_pos = self.pos, "SyncReader::read OK");
        } else {
            trace!(pos = self.pos, "SyncReader::read EOF");
        }
        Ok(n)
    }
}

impl<S> Seek for SyncReader<S>
where
    S: Source,
{
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        trace!(cur = self.pos, ?pos, "SyncReader::seek enter");

        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.source.len() else {
                    trace!("SyncReader::seek from end but source len unknown");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        StreamError::<S::Error>::UnknownLength.to_string(),
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            trace!(new_pos, "SyncReader::seek invalid (negative)");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        let new_pos_u64 = new_pos as u64;

        if let Some(len) = self.source.len()
            && new_pos_u64 > len
        {
            trace!(new_pos_u64, len, "SyncReader::seek invalid (past EOF)");
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                StreamError::<S::Error>::InvalidSeek.to_string(),
            ));
        }

        // Update position
        let old_pos = self.pos;
        self.pos = new_pos_u64;

        // If position changed, notify worker with new epoch
        if new_pos_u64 != old_pos {
            self.epoch = self.epoch.wrapping_add(1);
            self.current_chunk = None;
            self.eof_reached = false;

            // Send seek command to worker (blocking to ensure delivery)
            // Epoch is passed in command to ensure synchronization even if commands are lost
            let _ = self.cmd_tx.blocking_send(ByteSeekCmd {
                pos: new_pos_u64,
                epoch: self.epoch,
            });
            trace!(
                from = old_pos,
                to = new_pos_u64,
                epoch = self.epoch,
                "SyncReader::seek sent to worker"
            );
        }

        Ok(self.pos)
    }
}
