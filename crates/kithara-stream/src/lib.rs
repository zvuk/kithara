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

#[cfg(test)]
use std::time::Duration;
use std::{fmt, pin::Pin, sync::Arc};

use bytes::Bytes;
use futures::{Stream as FuturesStream, StreamExt};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

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

/// Errors produced by `kithara-stream` (generic over source error).
#[derive(Debug, Error)]
pub enum StreamError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    #[error("Seek not supported")]
    SeekNotSupported,

    #[error("Source error: {0}")]
    Source(#[source] E),

    #[error("Internal channel closed")]
    ChannelClosed,
}

pub type ByteStream<E> =
    Pin<Box<dyn FuturesStream<Item = Result<Bytes, StreamError<E>>> + Send + 'static>>;

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
    /// The underlying error type produced by this source.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The control message type emitted by this source/engine (optional).
    type Control: Send + 'static;

    /// Create a new byte stream starting from the current internal position (usually 0 initially).
    ///
    /// The returned stream must be `'static` because it is driven by a spawned task.
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

/// Stream returned from `Source::open`.
///
/// It yields either:
/// - `Message::Data(Bytes)` for byte payloads,
/// - `Message::Control(M)` for optional control/events.
pub type SourceStream<M, E> =
    Pin<Box<dyn FuturesStream<Item = Result<Message<M>, StreamError<E>>> + Send + 'static>>;

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

/// The orchestrator.
///
/// It spawns a task that:
/// - opens the source stream,
/// - forwards `Data(Bytes)` into the output channel,
/// - ignores or logs `Control(M)` for now (but can be surfaced later),
/// - listens for `SeekBytes` commands and re-opens the source as needed,
/// - stops when the consumer drops the output stream.
pub struct Stream<S>
where
    S: Source,
{
    handle: Handle,
    out: ReceiverStream<Result<Bytes, StreamError<S::Error>>>,
    _task: tokio::task::JoinHandle<()>,
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
    /// Start orchestration and return (`Stream`, `Handle`).
    ///
    /// The returned `Stream` yields bytes (`Bytes`) and ends when:
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
                            trace!("control message ignored (by design, for now)");
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

    pub fn into_byte_stream(self) -> ByteStream<S::Error> {
        Box::pin(self.out)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;

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
        type Error = Boom;
        type Control = ();

        fn open(
            &mut self,
            params: StreamParams,
        ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
            self.offline_mode_seen = Some(params.offline_mode);
            let start = self.pos;

            let items: Vec<Result<Message<()>, StreamError<Boom>>> = (0..4u8)
                .map(|i| Ok(Message::Data(Bytes::from(vec![start as u8 + i]))))
                .collect();

            Ok(Box::pin(futures::stream::iter(items)))
        }

        fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError<Boom>> {
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
        type Error = Boom;
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
            let start = self.pos;

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

        fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError<Boom>> {
            self.pos = pos;
            Ok(())
        }

        fn supports_seek(&self) -> bool {
            true
        }
    }

    struct NoSeekSource;

    impl Source for NoSeekSource {
        type Error = Boom;
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
            Ok(Box::pin(futures::stream::iter(vec![Ok(Message::Data(
                Bytes::from_static(b"abc"),
            ))])))
        }
    }

    struct FailingOpenSource;

    impl Source for FailingOpenSource {
        type Error = Boom;
        type Control = ();

        fn open(
            &mut self,
            _params: StreamParams,
        ) -> Result<SourceStream<Self::Control, Self::Error>, StreamError<Self::Error>> {
            Err(StreamError::Source(Boom))
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

        let first = s.next().await.unwrap().unwrap();
        assert_eq!(first, Bytes::from_static(&[0]));

        handle.seek_bytes::<Boom>(10).await.unwrap();

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

        let b = s.next().await.unwrap().unwrap();
        assert_eq!(b, Bytes::from_static(b"abc"));

        let err = handle.seek_bytes::<Boom>(1).await.unwrap_err();
        assert!(matches!(
            err,
            StreamError::ChannelClosed | StreamError::SeekNotSupported
        ));

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
