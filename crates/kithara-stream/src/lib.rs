//! `kithara-stream`
//!
//! This crate defines:
//! - `Source`: an abstract byte source (file, HLS, etc).
//! - `Stream`: an orchestration loop that consumes a `Source` and exposes an `async` byte stream,
//!   while accepting control commands (currently only seek-bytes).
//!
//! Notes:
//! - There is no explicit "stop" command. Stopping is done by dropping the output iterator/stream.
//! - `seek_bytes` is part of the contract, but implementations may return `SeekNotSupported`.
//! - `offline_mode` is a shared concern; the engine can expose it, while the concrete `Source`
//!   decides how to enforce it (cache-only, etc).

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

#[cfg(test)]
use std::time::Duration;

pub type ByteStream =
    Pin<Box<dyn FuturesStream<Item = Result<Bytes, StreamError>> + Send + 'static>>;

/// Data plane + control plane multiplexing.
///
/// You can use `M = ()` if you don't need control messages to be emitted from the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message<M> {
    Data(Bytes),
    Control(M),
}

/// Shared parameters for stream creation.
///
/// `offline_mode` is included because it is required for both file and HLS flows. Enforcement is
/// delegated to the `Source` implementation (e.g. cache-only, no network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    pub offline_mode: bool,
}

impl Default for StreamParams {
    fn default() -> Self {
        Self {
            offline_mode: false,
        }
    }
}

/// Commands sent to the orchestration loop.
///
/// There is intentionally no Stop command: dropping the consumer stream stops the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SeekBytes(u64),
}

/// Errors produced by `kithara-stream`.
#[derive(Debug, Error)]
pub enum StreamError {
    #[error("Seek not supported")]
    SeekNotSupported,

    #[error("Source error: {0}")]
    Source(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("Internal channel closed")]
    ChannelClosed,
}

impl StreamError {
    pub fn from_source<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Source(Box::new(e))
    }
}

/// Abstract byte source.
///
/// `Source` is responsible for:
/// - building the initial byte stream (potentially using network/cache),
/// - handling seeks (or rejecting them).
///
/// The orchestration loop (`Stream`) is responsible for:
/// - running the download/forwarding loop,
/// - listening to commands (seek),
/// - stopping when the consumer stream is dropped.
pub trait Source: Send + 'static {
    /// The control message type emitted by this source/engine (optional).
    type Control: Send + 'static;

    /// Create a new byte stream starting from the current internal position (usually 0 initially).
    ///
    /// The returned stream must be `'static` because it is driven by a spawned task.
    fn open(&mut self, params: StreamParams) -> Result<SourceStream<Self::Control>, StreamError>;

    /// Seek to the given byte position.
    ///
    /// Default: not supported.
    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError> {
        Err(StreamError::SeekNotSupported)
    }

    /// Whether this source supports `seek_bytes`.
    fn supports_seek(&self) -> bool {
        false
    }
}

/// Stream returned from `Source::open`.
///
/// It yields either:
/// - `Message::Data(Bytes)` for byte payloads,
/// - `Message::Control(M)` for optional control/events.
pub type SourceStream<M> =
    Pin<Box<dyn FuturesStream<Item = Result<Message<M>, StreamError>> + Send + 'static>>;

/// Handle for controlling a running stream.
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
    pub async fn seek_bytes(&self, pos: u64) -> Result<(), StreamError> {
        self.cmd_tx
            .send(Command::SeekBytes(pos))
            .await
            .map_err(|_| StreamError::ChannelClosed)
    }
}

/// The orchestrator.
///
/// It spawns a task that:
/// - opens the source stream,
/// - forwards `Data(Bytes)` into the output channel,
/// - ignores or logs `Control(M)` for now (but can be surfaced later),
/// - listens for `SeekBytes` commands and re-opens the source as needed,
/// - stops when the consumer drops the output stream.
///
/// Implementation detail:
/// - The orchestration task must keep the receiver side alive. If the receiver is dropped, the
///   sender observes "closed" and the loop exits immediately. Therefore we move the `ReceiverStream`
///   into the task and forward items into a second channel used by the public `Stream`.
pub struct Stream<S: Source> {
    handle: Handle,
    out: ReceiverStream<Result<Bytes, StreamError>>,
    _task: tokio::task::JoinHandle<()>,
    _keepalive: Arc<()>,
    _phantom: std::marker::PhantomData<S>,
}

impl<S: Source> fmt::Debug for Stream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl<S: Source> Stream<S> {
    /// Start orchestration and return (`Stream`, `Handle`).
    ///
    /// The returned `Stream` yields bytes (`Bytes`) and ends when:
    /// - the source ends (EOF), or
    /// - the consumer drops it.
    pub fn new(mut source: S, params: StreamParams) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(16);
        let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, StreamError>>(16);

        // Keepalive used to make it harder to accidentally drop all references while debugging;
        // not strictly necessary, but useful when expanding this crate.
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
                    // If nobody can send commands anymore, we continue streaming bytes until EOF
                    // (or until the consumer drops the output stream).
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
                            trace!("control message ignored (by design, for now)");
                        }
                    }
                    continue;
                }

                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else {
                            // All handles dropped. This must NOT stop the stream (stream-download keeps downloading);
                            // it only disables further control.
                            trace!("all command handles dropped; disabling command branch");
                            commands_closed = true;
                            continue;
                        };

                        match cmd {
                            Command::SeekBytes(pos) => {
                                debug!(pos, "seek command received");
                                if let Err(e) = source.seek_bytes(pos) {
                                    // Propagate seek failure but keep running.
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
                                trace!("control message ignored (by design, for now)");
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

    pub fn into_byte_stream(self) -> ByteStream {
        Box::pin(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[derive(Debug, Error)]
    #[error("boom")]
    struct Boom;

    struct FakeSource {
        supports_seek: bool,
        pos: u64,
        offline_mode_seen: Option<bool>,
    }

    impl FakeSource {
        fn new() -> Self {
            Self {
                supports_seek: true,
                pos: 0,
                offline_mode_seen: None,
            }
        }
    }

    impl Source for FakeSource {
        type Control = ();

        fn open(
            &mut self,
            params: StreamParams,
        ) -> Result<SourceStream<Self::Control>, StreamError> {
            self.offline_mode_seen = Some(params.offline_mode);
            let start = self.pos;

            // Produce deterministic bytes: first byte = start%256, then +1, +2, +3.
            let items: Vec<Result<Message<()>, StreamError>> = (0..4u8)
                .map(|i| Ok(Message::Data(Bytes::from(vec![start as u8 + i]))))
                .collect();

            Ok(Box::pin(futures::stream::iter(items)))
        }

        fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError> {
            if !self.supports_seek {
                return Err(StreamError::SeekNotSupported);
            }
            self.pos = pos;
            Ok(())
        }

        fn supports_seek(&self) -> bool {
            self.supports_seek
        }
    }

    struct SlowInfiniteSource {
        pos: u64,
    }

    impl SlowInfiniteSource {
        fn new() -> Self {
            Self { pos: 0 }
        }
    }

    impl Source for SlowInfiniteSource {
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control>, StreamError> {
            let start = self.pos;

            // Long-lived stream to avoid races where the driver reaches EOF before we can send seek.
            // Emits one byte every 10ms: start, start+1, start+2, ...
            Ok(Box::pin(async_stream::stream! {
                let mut i: u64 = 0;
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let b = Bytes::from(vec![(start.wrapping_add(i) % 256) as u8]);
                    yield Ok(Message::Data(b));
                    i = i.wrapping_add(1);
                }
            }))
        }

        fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError> {
            self.pos = pos;
            Ok(())
        }

        fn supports_seek(&self) -> bool {
            true
        }
    }

    struct NoSeekSource;

    impl Source for NoSeekSource {
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control>, StreamError> {
            Ok(Box::pin(futures::stream::iter(vec![Ok(Message::Data(
                Bytes::from_static(b"abc"),
            ))])))
        }
    }

    struct FailingOpenSource;

    impl Source for FailingOpenSource {
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control>, StreamError> {
            Err(StreamError::from_source(Boom))
        }
    }

    #[tokio::test]
    async fn forwards_bytes_from_source() {
        let stream = Stream::new(
            FakeSource::new(),
            StreamParams {
                offline_mode: false,
            },
        );
        let mut s = stream.into_byte_stream();

        let mut out = Vec::new();
        while let Some(item) = s.next().await {
            out.push(item.unwrap());
        }

        assert_eq!(
            out,
            vec![
                Bytes::from_static(&[0]),
                Bytes::from_static(&[1]),
                Bytes::from_static(&[2]),
                Bytes::from_static(&[3]),
            ]
        );
    }

    #[tokio::test]
    async fn seek_reopens_source_and_changes_output() {
        let stream = Stream::new(SlowInfiniteSource::new(), StreamParams::default());
        let handle = stream.handle();
        let mut s = stream.into_byte_stream();

        // Read first chunk(s) from initial position 0.
        let first = s.next().await.unwrap().unwrap();
        assert_eq!(first, Bytes::from_static(&[0]));

        // Seek to 10 and expect subsequent output to start from 10.
        handle.seek_bytes(10).await.unwrap();

        // Because the orchestration is concurrent, there may be some in-flight bytes.
        // The stream is long-lived, so we can deterministically wait until we observe [10].
        let mut seen_10 = false;
        for _ in 0..200 {
            let b = s.next().await.unwrap().unwrap();
            if b == Bytes::from_static(&[10]) {
                seen_10 = true;
                break;
            }
        }
        assert!(
            seen_10,
            "expected to observe bytes starting from seek position"
        );
    }

    #[tokio::test]
    async fn seek_on_non_seek_source_produces_error_but_stream_can_be_dropped() {
        let stream = Stream::new(NoSeekSource, StreamParams::default());
        let handle = stream.handle();
        let mut s = stream.into_byte_stream();

        // Consume the only "abc" chunk(s) from open.
        let b = s.next().await.unwrap().unwrap();
        assert_eq!(b, Bytes::from_static(b"abc"));

        // Seek should fail and be propagated as an output error if the loop is still running.
        // Depending on timing, the stream may already have ended, so just ensure seek returns error.
        let err = handle.seek_bytes(1).await.unwrap_err();
        assert!(matches!(
            err,
            StreamError::ChannelClosed | StreamError::SeekNotSupported
        ));

        // Dropping is the stop mechanism.
        drop(s);
    }

    #[tokio::test]
    async fn failing_open_is_propagated() {
        let stream = Stream::new(FailingOpenSource, StreamParams::default());
        let mut s = stream.into_byte_stream();

        let first = s.next().await.unwrap();
        assert!(matches!(first, Err(StreamError::Source(_))));
    }
}
