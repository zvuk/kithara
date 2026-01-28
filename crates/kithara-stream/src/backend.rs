#![forbid(unsafe_code)]

//! Generic backend for streaming sources.
//!
//! Backend wraps a `Source`, runs a worker loop calling `wait_range()` and `read_at()`,
//! and provides channel-based communication with sync `Reader`.

use std::sync::Arc;

use bytes::Bytes;
use kanal::{Receiver, Sender};
use kithara_bufpool::byte_pool;
use kithara_storage::WaitOutcome;
use parking_lot::Mutex;
use tracing::{debug, trace, warn};

use crate::source::Source;

/// Command sent from Reader to Backend.
#[derive(Debug, Clone)]
pub enum Command {
    /// Request read at offset.
    Read { offset: u64, len: usize },
    /// Seek to position (updates internal offset).
    Seek { offset: u64 },
    /// Stop the backend.
    Stop,
}

/// Response from Backend to Reader.
#[derive(Debug, Clone)]
pub enum Response {
    /// Data chunk.
    Data(Bytes),
    /// End of file reached.
    Eof,
    /// Error occurred.
    Error(String),
}

/// Trait for backend channel access (used by Reader).
pub trait BackendAccess: Send + Sync + 'static {
    /// Receiver for responses.
    fn response_rx(&self) -> &Receiver<Response>;

    /// Sender for commands.
    fn command_tx(&self) -> &Sender<Command>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;
}

/// Generic backend for any Source.
///
/// Spawns a worker that calls `source.wait_range()` and `source.read_at()`.
/// Implements `BackendAccess` for use with `Reader`.
pub struct Backend {
    cmd_tx: Sender<Command>,
    data_rx: Receiver<Response>,
    len: Arc<Mutex<Option<u64>>>,
}

impl Backend {
    /// Create new backend from a source.
    pub fn new<S: Source>(source: S) -> Self {
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);
        let len = Arc::new(Mutex::new(source.len()));

        tokio::spawn(Self::run_worker(source, cmd_rx, data_tx, len.clone()));

        Self {
            cmd_tx,
            data_rx,
            len,
        }
    }

    async fn run_worker<S: Source>(
        source: S,
        cmd_rx: Receiver<Command>,
        data_tx: Sender<Response>,
        len: Arc<Mutex<Option<u64>>>,
    ) {
        debug!("Backend worker started");

        let pool = byte_pool();

        loop {
            tokio::select! {
                biased;

                cmd = cmd_rx.as_async().recv() => {
                    let cmd = match cmd {
                        Ok(cmd) => cmd,
                        Err(_) => {
                            debug!("Backend cmd channel closed");
                            break;
                        }
                    };

                    match cmd {
                        Command::Read { offset, len: read_len } => {
                            trace!(offset, read_len, "Backend read request");

                            let range = offset..offset + read_len as u64;

                            // Wait for data to be available
                            let wait_result = source.wait_range(range.clone()).await;
                            let outcome = match wait_result {
                                Ok(outcome) => outcome,
                                Err(e) => {
                                    warn!(?e, "Backend wait_range error");
                                    if data_tx.as_async().send(Response::Error(e.to_string())).await.is_err() {
                                        debug!("Backend data channel closed");
                                        break;
                                    }
                                    continue;
                                }
                            };

                            // Handle EOF
                            if matches!(outcome, WaitOutcome::Eof) {
                                if data_tx.as_async().send(Response::Eof).await.is_err() {
                                    debug!("Backend data channel closed");
                                    break;
                                }
                                continue;
                            }

                            // Get buffer from pool and read data
                            let mut buf = pool.get();
                            buf.resize(read_len, 0);

                            match source.read_at(offset, &mut buf).await {
                                Ok(0) => {
                                    if data_tx.as_async().send(Response::Eof).await.is_err() {
                                        debug!("Backend data channel closed");
                                        break;
                                    }
                                }
                                Ok(n) => {
                                    // Update len if we now know it
                                    if source.len().is_some() {
                                        *len.lock() = source.len();
                                    }
                                    // Copy to Bytes and return pooled buffer
                                    let bytes = Bytes::copy_from_slice(&buf[..n]);
                                    drop(buf); // Return to pool before sending
                                    if data_tx.as_async().send(Response::Data(bytes)).await.is_err() {
                                        debug!("Backend data channel closed");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!(?e, "Backend read_at error");
                                    if data_tx.as_async().send(Response::Error(e.to_string())).await.is_err() {
                                        debug!("Backend data channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                        Command::Seek { offset } => {
                            trace!(offset, "Backend seek (no-op, Reader handles position)");
                            // Seek is handled by Reader updating its position.
                            // Backend just serves read requests at specified offsets.
                        }
                        Command::Stop => {
                            debug!("Backend stop");
                            break;
                        }
                    }
                }
            }
        }

        debug!("Backend worker stopped");
    }
}

impl BackendAccess for Backend {
    fn response_rx(&self) -> &Receiver<Response> {
        &self.data_rx
    }

    fn command_tx(&self) -> &Sender<Command> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        *self.len.lock()
    }
}
