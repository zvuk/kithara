#![forbid(unsafe_code)]

//! Generic backend for async sources.
//!
//! `Backend<S>` wraps any `Source<Item=u8>` and provides channel-based
//! communication for use with sync `Reader`.

use std::sync::Arc;

use bytes::Bytes;
use kanal::{Receiver, Sender};
use kithara_storage::WaitOutcome;
use tracing::{debug, trace};

use crate::Source;

/// Command sent from reader to backend.
#[derive(Debug, Clone)]
pub enum Command {
    /// Request read at offset.
    Read { offset: u64, len: usize },
    /// Seek to new position.
    Seek { offset: u64 },
    /// Stop the backend.
    Stop,
}

/// Response from backend to reader.
#[derive(Debug, Clone)]
pub enum Response {
    /// Data chunk.
    Data(Bytes),
    /// End of file reached.
    Eof,
    /// Error occurred.
    Error(String),
}

/// Trait for backend channel access.
///
/// Internal trait used by `Reader` to communicate with backend.
pub trait BackendAccess: Send + Sync + 'static {
    /// Receiver for data responses.
    fn response_rx(&self) -> &Receiver<Response>;

    /// Sender for commands.
    fn command_tx(&self) -> &Sender<Command>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;
}

/// Generic backend for any `Source<Item=u8>`.
///
/// Spawns an async worker that handles read commands using the source.
pub struct Backend<S: Source<Item = u8>> {
    cmd_tx: Sender<Command>,
    data_rx: Receiver<Response>,
    source: Arc<S>,
}

impl<S: Source<Item = u8>> Backend<S> {
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
        cmd_rx: Receiver<Command>,
        data_tx: Sender<Response>,
    ) {
        debug!("Backend worker started");

        loop {
            let cmd = match cmd_rx.as_async().recv().await {
                Ok(cmd) => cmd,
                Err(_) => {
                    debug!("Backend cmd channel closed");
                    break;
                }
            };

            match cmd {
                Command::Read { offset, len } => {
                    let response = Self::handle_read(&source, offset, len).await;

                    if data_tx.as_async().send(response).await.is_err() {
                        debug!("Backend data channel closed");
                        break;
                    }
                }
                Command::Seek { offset } => {
                    trace!(offset, "Backend seek command");
                }
                Command::Stop => {
                    debug!("Backend stop command");
                    break;
                }
            }
        }

        debug!("Backend worker stopped");
    }

    async fn handle_read(source: &S, offset: u64, len: usize) -> Response {
        trace!(offset, len, "Backend read request");

        let range = offset..offset.saturating_add(len as u64);
        match source.wait_range(range).await {
            Ok(WaitOutcome::Ready) => {}
            Ok(WaitOutcome::Eof) => {
                return Response::Eof;
            }
            Err(e) => {
                return Response::Error(format!("wait_range error: {}", e));
            }
        }

        let mut buf = vec![0u8; len];
        match source.read_at(offset, &mut buf).await {
            Ok(0) => Response::Eof,
            Ok(n) => {
                buf.truncate(n);
                trace!(offset, bytes = n, "Backend read complete");
                Response::Data(Bytes::from(buf))
            }
            Err(e) => Response::Error(format!("read_at error: {}", e)),
        }
    }
}

impl<S: Source<Item = u8>> BackendAccess for Backend<S> {
    fn response_rx(&self) -> &Receiver<Response> {
        &self.data_rx
    }

    fn command_tx(&self) -> &Sender<Command> {
        &self.cmd_tx
    }

    fn len(&self) -> Option<u64> {
        self.source.len()
    }
}
