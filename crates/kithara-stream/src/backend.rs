#![forbid(unsafe_code)]

//! Generic backend for streaming sources.
//!
//! Backend wraps a `Source` and `Downloader`, runs a worker loop, and provides
//! channel-based communication with sync `Reader`.

use std::sync::Arc;

use bytes::Bytes;
use kanal::{Receiver, Sender};
use kithara_bufpool::byte_pool;
use kithara_storage::WaitOutcome;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::{downloader::Downloader, media::MediaInfo, source::Source};

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
#[expect(clippy::len_without_is_empty)]
pub trait BackendAccess: Send + Sync + 'static {
    /// Receiver for responses.
    fn response_rx(&self) -> &Receiver<Response>;

    /// Sender for commands.
    fn command_tx(&self) -> &Sender<Command>;

    /// Total length if known.
    fn len(&self) -> Option<u64>;

    /// Media info if known.
    fn media_info(&self) -> Option<MediaInfo>;

    /// Current segment byte range.
    fn current_segment_range(&self) -> std::ops::Range<u64>;
}

/// Shared mutable state between worker and Backend accessors.
#[derive(Clone)]
struct SharedState {
    len: Arc<Mutex<Option<u64>>>,
    media_info: Arc<Mutex<Option<MediaInfo>>>,
    segment_range: Arc<Mutex<std::ops::Range<u64>>>,
}

/// Generic backend for any Source + Downloader.
///
/// Spawns a worker that calls `source.wait_range()` and `source.read_at()`,
/// driving the downloader in parallel for look-ahead fetching.
/// Implements `BackendAccess` for use with `Reader`.
pub struct Backend {
    cmd_tx: Sender<Command>,
    data_rx: Receiver<Response>,
    shared: SharedState,
}

impl Backend {
    /// Create new backend from a source, downloader, and cancellation token.
    pub fn new<S: Source, D: Downloader>(
        source: S,
        downloader: D,
        cancel: CancellationToken,
    ) -> Self {
        let (cmd_tx, cmd_rx) = kanal::bounded(4);
        let (data_tx, data_rx) = kanal::bounded(4);
        let shared = SharedState {
            len: Arc::new(Mutex::new(source.len())),
            media_info: Arc::new(Mutex::new(source.media_info())),
            segment_range: Arc::new(Mutex::new(source.current_segment_range())),
        };

        tokio::spawn(Self::run_worker(
            source,
            downloader,
            cancel,
            cmd_rx,
            data_tx,
            shared.clone(),
        ));

        Self {
            cmd_tx,
            data_rx,
            shared,
        }
    }

    async fn run_worker<S: Source, D: Downloader>(
        mut source: S,
        downloader: D,
        cancel: CancellationToken,
        cmd_rx: Receiver<Command>,
        data_tx: Sender<Response>,
        shared: SharedState,
    ) {
        debug!("Backend worker started");

        // Spawn downloader in a separate task so it runs independently
        // and is not cancelled by incoming read commands.
        let dl_cancel = cancel.clone();
        tokio::spawn(async move {
            Self::run_downloader(downloader, dl_cancel).await;
        });

        let pool = byte_pool();

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!("Backend cancelled");
                    break;
                }

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

                            // Wait for data with cancel support
                            let wait_result = tokio::select! {
                                biased;
                                () = cancel.cancelled() => {
                                    debug!("Backend cancelled during wait_range");
                                    break;
                                }
                                result = source.wait_range(range.clone()) => result,
                            };

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
                                    if source.len().is_some() {
                                        *shared.len.lock() = source.len();
                                    }
                                    if let Some(info) = source.media_info() {
                                        *shared.media_info.lock() = Some(info);
                                    }
                                    *shared.segment_range.lock() = source.current_segment_range();
                                    let bytes = Bytes::copy_from_slice(&buf[..n]);
                                    drop(buf);
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

    async fn run_downloader<D: Downloader>(mut downloader: D, cancel: CancellationToken) {
        debug!("Downloader task started");
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    debug!("Downloader cancelled");
                    break;
                }
                has_more = downloader.step() => {
                    if !has_more {
                        debug!("Downloader complete");
                        break;
                    }
                }
            }
        }
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
        *self.shared.len.lock()
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.shared.media_info.lock().clone()
    }

    fn current_segment_range(&self) -> std::ops::Range<u64> {
        self.shared.segment_range.lock().clone()
    }
}
