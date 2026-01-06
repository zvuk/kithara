#![forbid(unsafe_code)]

//! Legacy `Source` + `Stream` orchestrator.
//!
//! This module exists for transitional compatibility while higher-level crates migrate to the
//! new `Engine` / `EngineSource` API.
//!
//! It drives a `Source` by spawning a task that:
//! - opens the source stream,
//! - forwards `Message::Data(Bytes)` into an output channel,
//! - listens for `Command::SeekBytes` and re-opens the source as needed,
//! - stops when the consumer drops the output stream.
//!
//! Notes:
//! - Control messages are currently ignored by design.
//! - Stopping is done by dropping the output stream (no explicit stop command).

use std::{fmt, pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

use crate::{Message, StreamError, StreamParams};

/// Commands sent to the legacy orchestration loop.
///
/// There is intentionally no Stop command: dropping the consumer stream stops the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SeekBytes(u64),
}

/// Stream returned from `Source::open`.
///
/// It yields either:
/// - `Message::Data(Bytes)` for byte payloads,
/// - `Message::Control(M)` for optional control/events.
///
/// Note: the returned stream must be `'static` because it is driven by a spawned task.
pub type SourceStream<M, E> =
    Pin<Box<dyn FuturesStream<Item = Result<Message<M>, StreamError<E>>> + Send + 'static>>;

/// A byte-only stream produced by the legacy orchestrator.
pub type ByteStream<E> =
    Pin<Box<dyn FuturesStream<Item = Result<Bytes, StreamError<E>>> + Send + 'static>>;

/// Abstract byte source (legacy).
///
/// `Source` is responsible for:
/// - building the initial byte stream (potentially using network/cache),
/// - handling seeks (or rejecting them).
///
/// The orchestration loop (`Stream`) is responsible for:
/// - driving the source stream,
/// - listening to commands (seek),
/// - stopping when the consumer drops the output stream.
pub trait Source: Send + 'static {
    /// The underlying error type produced by this source.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The control message type emitted by this source.
    type Control: Send + 'static;

    /// Create a new byte stream starting from the current internal position (usually 0 initially).
    fn open(
        &mut self,
        params: StreamParams,
    ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>>;

    /// Seek to the given byte position.
    ///
    /// Default: not supported.
    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
        Err(StreamError::SeekNotSupported)
    }

    /// Whether this source supports `seek_bytes`.
    fn supports_seek(&self) -> bool {
        false
    }
}

/// Handle for controlling a running legacy stream.
#[derive(Clone)]
pub struct Handle {
    cmd_tx: mpsc::Sender<Command>,
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle").finish_non_exhaustive()
    }
}

impl Handle {
    pub async fn seek_bytes<E>(&self, pos: u64) -> Result<(), StreamError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.cmd_tx
            .send(Command::SeekBytes(pos))
            .await
            .map_err(|_| StreamError::ChannelClosed)
    }
}

/// The legacy orchestrator.
///
/// It spawns a task that:
/// - opens the source stream,
/// - forwards `Data(Bytes)` into the output channel,
/// - ignores `Control(M)` (by design, for now),
/// - listens for `SeekBytes` commands and re-opens the source as needed,
/// - stops when the consumer drops the output stream.
pub struct Stream<S>
where
    S: Source,
{
    handle: Handle,
    out: ReceiverStream<Result<Bytes, StreamError<S::Error>>>,
    _task: tokio::task::JoinHandle<()>,
    // Keepalive ensures the task isn't dropped prematurely if the stream is moved around.
    _keepalive: Arc<()>,
    _phantom: std::marker::PhantomData<S>,
}

impl<S> fmt::Debug for Stream<S>
where
    S: Source,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl<S> Stream<S>
where
    S: Source,
{
    /// Start orchestration and return the stream.
    ///
    /// The returned stream yields bytes (`Bytes`) and ends when:
    /// - the source ends (EOF), or
    /// - the consumer drops it.
    pub fn new(mut source: S, params: StreamParams) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(16);
        let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, StreamError<S::Error>>>(16);

        let keepalive = Arc::new(());
        let keepalive_task = Arc::clone(&keepalive);

        let task = tokio::spawn(async move {
            let _keepalive = keepalive_task;

            let mut current = match source.open(params) {
                Ok(s) => s,
                Err(e) => {
                    let _ = out_tx.send(Err(e)).await;
                    return;
                }
            };

            let mut commands_closed = false;

            loop {
                // Stop mechanism: if consumer dropped the public stream, we stop.
                if out_tx.is_closed() {
                    trace!("consumer dropped output stream; stopping");
                    return;
                }

                if commands_closed {
                    match current.next().await {
                        None => {
                            trace!("source stream ended");
                            return;
                        }
                        Some(Err(e)) => {
                            let _ = out_tx.send(Err(e)).await;
                            return;
                        }
                        Some(Ok(Message::Data(b))) => {
                            if out_tx.send(Ok(b)).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Control(_m))) => {
                            trace!("control message ignored (legacy behavior)");
                        }
                    }
                    continue;
                }

                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else {
                            trace!("all command handles dropped; disabling command branch");
                            commands_closed = true;
                            continue;
                        };

                        match cmd {
                            Command::SeekBytes(pos) => {
                                debug!(pos, "seek command received");
                                if let Err(e) = source.seek_bytes(pos) {
                                    let _ = out_tx.send(Err(e)).await;
                                    continue;
                                }

                                match source.open(params) {
                                    Ok(s) => current = s,
                                    Err(e) => {
                                        let _ = out_tx.send(Err(e)).await;
                                        return;
                                    }
                                }
                            }
                        }
                    }

                    item = current.next() => {
                        match item {
                            None => {
                                trace!("source stream ended");
                                return;
                            }
                            Some(Err(e)) => {
                                let _ = out_tx.send(Err(e)).await;
                                return;
                            }
                            Some(Ok(Message::Data(b))) => {
                                if out_tx.send(Ok(b)).await.is_err() {
                                    return;
                                }
                            }
                            Some(Ok(Message::Control(_m))) => {
                                trace!("control message ignored (legacy behavior)");
                            }
                        }
                    }
                }
            }
        });

        Self {
            handle: Handle { cmd_tx },
            out: ReceiverStream::new(out_rx),
            _task: task,
            _keepalive: keepalive,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn into_byte_stream(self) -> ByteStream<S::Error> {
        Box::pin(self.out)
    }
}
