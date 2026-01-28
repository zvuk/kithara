#![forbid(unsafe_code)]

//! Channel-based sync reader with random access.
//!
//! `ChannelReader<B>` implements `Read + Seek` using a `RandomAccessBackend`.
//! No `block_on` - uses kanal's native sync operations.

use std::io::{Read, Seek, SeekFrom};

use bytes::Bytes;
use kanal::{Receiver, Sender};

/// Command sent from reader to backend.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    /// Request read at offset.
    Read { offset: u64, len: usize },
    /// Seek to new position (backend should reset state).
    Seek { offset: u64 },
    /// Stop the backend.
    Stop,
}

/// Response from backend.
#[derive(Debug, Clone)]
pub enum BackendResponse {
    /// Data chunk.
    Data(Bytes),
    /// End of file reached.
    Eof,
    /// Error occurred.
    Error(String),
}

/// Backend for random access via channels.
///
/// Implemented by file/hls to provide async reading with sync interface.
pub trait RandomAccessBackend: Send + Sync + 'static {
    /// Receiver for data responses.
    fn data_rx(&self) -> &Receiver<BackendResponse>;

    /// Sender for commands.
    fn cmd_tx(&self) -> &Sender<BackendCommand>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;
}

/// Channel-based sync reader with random access.
///
/// Uses `RandomAccessBackend` trait for actual I/O.
/// Implements `Read + Seek` for use with symphonia.
pub struct ChannelReader<B: RandomAccessBackend> {
    backend: B,
    pos: u64,
    buffer: Bytes,
    buffer_offset: usize,
    eof: bool,
}

impl<B: RandomAccessBackend> ChannelReader<B> {
    /// Create new channel reader.
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            pos: 0,
            buffer: Bytes::new(),
            buffer_offset: 0,
            eof: false,
        }
    }

    /// Get current position.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Get total length if known.
    pub fn len(&self) -> Option<u64> {
        self.backend.len()
    }

    /// Check if length is zero or unknown.
    pub fn is_empty(&self) -> bool {
        self.backend.len().map(|l| l == 0).unwrap_or(true)
    }

    /// Request data from backend and wait for response.
    fn request_and_receive(&mut self, len: usize) -> std::io::Result<Option<Bytes>> {
        // Send read request
        self.backend
            .cmd_tx()
            .send(BackendCommand::Read {
                offset: self.pos,
                len,
            })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "backend stopped")
            })?;

        // Wait for response (sync, blocking)
        match self.backend.data_rx().recv() {
            Ok(BackendResponse::Data(bytes)) => Ok(Some(bytes)),
            Ok(BackendResponse::Eof) => Ok(None),
            Ok(BackendResponse::Error(msg)) => {
                Err(std::io::Error::new(std::io::ErrorKind::Other, msg))
            }
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "backend channel closed",
            )),
        }
    }
}

impl<B: RandomAccessBackend> Read for ChannelReader<B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.eof {
            return Ok(0);
        }

        // First, drain any buffered data
        if self.buffer_offset < self.buffer.len() {
            let remaining = &self.buffer[self.buffer_offset..];
            let to_copy = remaining.len().min(buf.len());
            buf[..to_copy].copy_from_slice(&remaining[..to_copy]);
            self.buffer_offset += to_copy;
            self.pos += to_copy as u64;
            return Ok(to_copy);
        }

        // Buffer exhausted, request new data
        match self.request_and_receive(buf.len())? {
            Some(bytes) => {
                if bytes.is_empty() {
                    self.eof = true;
                    return Ok(0);
                }

                let to_copy = bytes.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&bytes[..to_copy]);
                self.pos += to_copy as u64;

                // Buffer remaining if any
                if to_copy < bytes.len() {
                    self.buffer = bytes;
                    self.buffer_offset = to_copy;
                } else {
                    self.buffer = Bytes::new();
                    self.buffer_offset = 0;
                }

                Ok(to_copy)
            }
            None => {
                self.eof = true;
                Ok(0)
            }
        }
    }
}

impl<B: RandomAccessBackend> Seek for ChannelReader<B> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(delta) => (self.pos as i128).saturating_add(delta as i128),
            SeekFrom::End(delta) => {
                let Some(len) = self.backend.len() else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                (len as i128).saturating_add(delta as i128)
            }
        };

        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }

        let new_pos = new_pos as u64;

        if let Some(len) = self.backend.len() {
            if new_pos > len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "seek past EOF",
                ));
            }
        }

        // Send seek command to backend
        self.backend
            .cmd_tx()
            .send(BackendCommand::Seek { offset: new_pos })
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "backend stopped")
            })?;

        // Clear buffer and reset state
        self.buffer = Bytes::new();
        self.buffer_offset = 0;
        self.pos = new_pos;
        self.eof = false;

        Ok(new_pos)
    }
}

impl<B: RandomAccessBackend> Drop for ChannelReader<B> {
    fn drop(&mut self) {
        // Best-effort stop signal
        let _ = self.backend.cmd_tx().send(BackendCommand::Stop);
    }
}

// ============================================================================
// ChannelBackend - generic backend for any Source<Item=u8>
// ============================================================================

use std::sync::Arc;

use kithara_storage::WaitOutcome;
use tracing::{debug, trace};

use crate::Source;

/// Generic backend for any `Source<Item=u8>`.
///
/// Spawns an async worker that handles read commands using the source.
/// Implements `RandomAccessBackend` for use with `ChannelReader`.
pub struct ChannelBackend<S: Source<Item = u8>> {
    cmd_tx: Sender<BackendCommand>,
    data_rx: Receiver<BackendResponse>,
    source: Arc<S>,
}

impl<S: Source<Item = u8>> ChannelBackend<S> {
    /// Create new backend from a source.
    ///
    /// Spawns async worker that handles read commands.
    pub fn new(source: S) -> Self {
        let source = Arc::new(source);
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);

        tokio::spawn(Self::run_worker(source.clone(), cmd_rx, data_tx));

        Self {
            cmd_tx,
            data_rx,
            source,
        }
    }

    /// Get reference to the underlying source.
    pub fn source(&self) -> &S {
        &self.source
    }

    async fn run_worker(
        source: Arc<S>,
        cmd_rx: Receiver<BackendCommand>,
        data_tx: Sender<BackendResponse>,
    ) {
        debug!("ChannelBackend worker started");

        loop {
            let cmd = match cmd_rx.as_async().recv().await {
                Ok(cmd) => cmd,
                Err(_) => {
                    debug!("ChannelBackend cmd channel closed");
                    break;
                }
            };

            match cmd {
                BackendCommand::Read { offset, len } => {
                    let response = Self::handle_read(&source, offset, len).await;

                    if data_tx.as_async().send(response).await.is_err() {
                        debug!("ChannelBackend data channel closed");
                        break;
                    }
                }
                BackendCommand::Seek { offset } => {
                    trace!(offset, "ChannelBackend seek command");
                    // Seek is handled by read offset - no special action needed
                }
                BackendCommand::Stop => {
                    debug!("ChannelBackend stop command");
                    break;
                }
            }
        }

        debug!("ChannelBackend worker stopped");
    }

    async fn handle_read(source: &S, offset: u64, len: usize) -> BackendResponse {
        trace!(offset, len, "ChannelBackend read request");

        // Wait for range to be available
        let range = offset..offset.saturating_add(len as u64);
        match source.wait_range(range).await {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => {
                return BackendResponse::Eof;
            }
            Err(e) => {
                return BackendResponse::Error(format!("wait_range error: {}", e));
            }
        }

        // Read data
        let mut buf = vec![0u8; len];
        match source.read_at(offset, &mut buf).await {
            Ok(0) => BackendResponse::Eof,
            Ok(n) => {
                buf.truncate(n);
                trace!(offset, bytes = n, "ChannelBackend read complete");
                BackendResponse::Data(Bytes::from(buf))
            }
            Err(e) => BackendResponse::Error(format!("read_at error: {}", e)),
        }
    }
}

impl<S: Source<Item = u8>> RandomAccessBackend for ChannelBackend<S> {
    fn data_rx(&self) -> &Receiver<BackendResponse> {
        &self.data_rx
    }

    fn cmd_tx(&self) -> &Sender<BackendCommand> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        self.source.len()
    }
}
