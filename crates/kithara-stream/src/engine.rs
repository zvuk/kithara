#![forbid(unsafe_code)]

use std::{fmt, pin::Pin};

use futures::{Stream as FuturesStream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

use crate::{StreamError, StreamMsg, StreamParams};

/// Commands accepted by [`Engine`].
///
/// Higher-level crates can wrap these commands into their own API surfaces.
/// The engine currently only needs byte-seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCommand {
    SeekBytes(u64),
}

/// A handle for controlling a running [`Engine`].
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<EngineCommand>,
}

impl fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineHandle").finish_non_exhaustive()
    }
}

impl EngineHandle {
    pub async fn seek_bytes<E>(&self, pos: u64) -> Result<(), StreamError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.cmd_tx
            .send(EngineCommand::SeekBytes(pos))
            .await
            .map_err(|_| StreamError::ChannelClosed)
    }
}

/// Convenience alias for the engine output stream type.
pub type EngineStream<C, Ev, E> =
    Pin<Box<dyn FuturesStream<Item = Result<StreamMsg<C, Ev>, StreamError<E>>> + Send + 'static>>;

/// Writer task handle type returned by an [`EngineSource`].
///
/// The task is expected to:
/// - fill underlying storage/resources
/// - materialize errors into the storage layer (preferred), or return `Err(E)`
///
/// The engine treats:
/// - successful completion as "writer done" and continues draining reader until EOF
/// - error completion as a terminal error surfaced to the consumer
pub type WriterTask<E> = tokio::task::JoinHandle<Result<(), E>>;

/// A source contract for the new engine.
///
/// Key requirement (normative):
/// - The engine must be able to create a *single owned session state* and then construct both:
///   - a writer task that fills storage/resources
///   - a reader stream that drains storage/resources
///   from that same session state.
///
/// Why owned?
/// - Both the writer task and the reader stream are driven by a spawned task and therefore must
///   be `'static`.
/// - Borrowing `&mut self` into either would allow borrowed data to escape and/or force
///   non-`'static` lifetimes, which breaks the deadlock-free "spawned engine" model.
///
/// Design:
/// - `Control` and `Event` are fully generic and defined by higher-level crates.
/// - The engine does not interpret `Control`/`Event`; it just forwards them.
pub trait EngineSource: Send + 'static {
    type Error: std::error::Error + Send + Sync + 'static;
    type Control: Send + 'static;
    type Event: Send + 'static;

    /// Owned session state shared by writer and reader.
    ///
    /// Typical examples:
    /// - a handle to an assets-backed streaming resource + cancellation token
    /// - any planner state needed to map logical offsets to underlying resources (HLS)
    type State: Send + Sync + 'static;

    /// Create (or reuse) the owned session state for this source.
    ///
    /// This is called exactly once by the engine at startup.
    fn init(
        &mut self,
        params: StreamParams,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<Self::State, StreamError<Self::Error>>>
                + Send
                + 'static,
        >,
    >;

    /// Build a reader stream from the owned session `state`.
    ///
    /// This stream is expected to produce:
    /// - `StreamMsg::Data(Bytes)` for data,
    /// - optional `StreamMsg::Control(C)` and `StreamMsg::Event(Ev)` messages,
    /// and should terminate on EOF.
    fn open_reader(
        &mut self,
        state: &Self::State,
        params: StreamParams,
    ) -> Result<EngineStream<Self::Control, Self::Event, Self::Error>, StreamError<Self::Error>>;

    /// Start (or obtain) the writer task that fills the underlying storage/resources.
    ///
    /// This must be called exactly once by the engine per `Engine::new` instance.
    fn start_writer(
        &mut self,
        state: &Self::State,
        params: StreamParams,
    ) -> Result<WriterTask<Self::Error>, StreamError<Self::Error>>;

    /// Seek to a byte position (optional).
    ///
    /// If seek is supported, the source should update its internal position such that the next
    /// `open_reader` starts reading from the requested position.
    fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
        Err(StreamError::SeekNotSupported)
    }

    fn supports_seek(&self) -> bool {
        true
    }
}

/// The new generic engine.
///
/// It orchestrates:
/// - one writer task (fetch → storage)
/// - one reader stream (storage → consumer)
/// - an optional command channel for `seek`
///
/// Deadlock avoidance (design notes):
/// - The engine uses fair `tokio::select!` (no `biased;`) when polling reader + commands.
/// - The writer is *not* polled in a tight loop; instead we await it via a single future and mark
///   it done when it completes.
/// - The engine stops promptly when the consumer drops the output stream (`out_tx.is_closed()`).
pub struct Engine<S>
where
    S: EngineSource,
{
    handle: EngineHandle,
    out: ReceiverStream<Result<StreamMsg<S::Control, S::Event>, StreamError<S::Error>>>,
    _task: tokio::task::JoinHandle<()>,
    _phantom: std::marker::PhantomData<S>,
}

impl<S> fmt::Debug for Engine<S>
where
    S: EngineSource,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl<S> Engine<S>
where
    S: EngineSource,
{
    pub fn new(mut source: S, params: StreamParams) -> Self {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<EngineCommand>(16);
        let (out_tx, out_rx) =
            mpsc::channel::<Result<StreamMsg<S::Control, S::Event>, StreamError<S::Error>>>(32);

        let task = tokio::spawn(async move {
            // Initialize owned session state once. Both writer and reader are derived from it.
            let state = match source.init(params).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = out_tx.send(Err(e)).await;
                    return;
                }
            };

            // Start the writer once (from the owned state).
            let mut writer: Option<WriterTask<S::Error>> = match source.start_writer(&state, params)
            {
                Ok(h) => Some(h),
                Err(e) => {
                    let _ = out_tx.send(Err(e)).await;
                    return;
                }
            };

            // Open the reader once; on seek we re-open (from the same owned state).
            let mut reader = match source.open_reader(&state, params) {
                Ok(s) => s,
                Err(e) => {
                    let _ = out_tx.send(Err(e)).await;
                    return;
                }
            };

            let mut writer_done = false;
            let mut commands_closed = false;

            loop {
                // Drop-to-stop.
                if out_tx.is_closed() {
                    if let Some(w) = writer.take() {
                        w.abort();
                        let _ = w.await;
                    }
                    return;
                }

                tokio::select! {
                    // Writer completion branch (one-shot)
                    w = async {
                        match writer.as_mut() {
                            Some(h) => Some(h.await),
                            None => None,
                        }
                    }, if !writer_done => {
                        writer_done = true;
                        let _ = writer.take(); // ensure we never await/abort again

                        match w {
                            None => {}
                            Some(Ok(Ok(()))) => {
                                trace!("Engine writer completed successfully");
                            }
                            Some(Ok(Err(e))) => {
                                let _ = out_tx.send(Err(StreamError::Source(e))).await;
                                return;
                            }
                            Some(Err(join_err)) => {
                                let _ = out_tx
                                    .send(Err(StreamError::WriterJoin(join_err.to_string())))
                                    .await;
                                return;
                            }
                        }
                    }

                    // Commands (optional)
                    cmd = cmd_rx.recv(), if !commands_closed => {
                        let Some(cmd) = cmd else {
                            commands_closed = true;
                            continue;
                        };

                        match cmd {
                            EngineCommand::SeekBytes(pos) => {
                                debug!(pos, "Engine seek command received");
                                if let Err(e) = source.seek_bytes(pos) {
                                    let _ = out_tx.send(Err(e)).await;
                                    continue;
                                }
                                match source.open_reader(&state, params) {
                                    Ok(s) => reader = s,
                                    Err(e) => {
                                        let _ = out_tx.send(Err(e)).await;
                                        return;
                                    }
                                }
                            }
                        }
                    }

                    // Reader items
                    item = reader.next() => {
                        match item {
                            None => return,
                            Some(item) => {
                                if out_tx.send(item).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });

        Self {
            handle: EngineHandle { cmd_tx },
            out: ReceiverStream::new(out_rx),
            _task: task,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    pub fn into_stream(self) -> EngineStream<S::Control, S::Event, S::Error> {
        Box::pin(self.out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    use bytes::Bytes;
    use futures::{StreamExt, stream};
    use tokio::time::{Duration, sleep};

    use super::*;

    #[derive(Debug)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("test error")
        }
    }

    impl std::error::Error for TestError {}

    struct SeekState {
        pos: AtomicU64,
        data: Vec<u8>,
    }

    struct SeekSource {
        data: Vec<u8>,
        state: Option<Arc<SeekState>>,
    }

    impl SeekSource {
        fn new(data: Vec<u8>) -> Self {
            Self { data, state: None }
        }
    }

    impl EngineSource for SeekSource {
        type Error = TestError;
        type Control = ();
        type Event = ();
        type State = Arc<SeekState>;

        fn init(
            &mut self,
            _params: StreamParams,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = Result<Self::State, StreamError<Self::Error>>>
                    + Send
                    + 'static,
            >,
        > {
            let state = Arc::new(SeekState {
                pos: AtomicU64::new(0),
                data: self.data.clone(),
            });
            self.state = Some(state.clone());
            Box::pin(async move { Ok(state) })
        }

        fn open_reader(
            &mut self,
            state: &Self::State,
            _params: StreamParams,
        ) -> Result<EngineStream<Self::Control, Self::Event, Self::Error>, StreamError<Self::Error>>
        {
            let start = state.pos.load(Ordering::SeqCst) as usize;
            let data = state.data.clone();
            let items = data
                .into_iter()
                .skip(start)
                .map(|b| Ok(StreamMsg::Data(Bytes::from(vec![b]))));
            Ok(stream::iter(items).boxed())
        }

        fn start_writer(
            &mut self,
            _state: &Self::State,
            _params: StreamParams,
        ) -> Result<WriterTask<Self::Error>, StreamError<Self::Error>> {
            Ok(tokio::spawn(async { Ok(()) }))
        }

        fn seek_bytes(&mut self, pos: u64) -> Result<(), StreamError<Self::Error>> {
            if let Some(state) = &self.state {
                state.pos.store(pos, Ordering::SeqCst);
            }
            Ok(())
        }

        fn supports_seek(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn seek_reopens_reader_after_writer_done() {
        let engine = Engine::new(SeekSource::new(b"abcd".to_vec()), StreamParams::default());
        let handle = engine.handle();
        let mut stream = engine.into_stream();

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first, StreamMsg::Data(Bytes::from_static(b"a")));

        handle.seek_bytes::<TestError>(2).await.unwrap();

        let second = stream.next().await.unwrap().unwrap();
        assert_eq!(second, StreamMsg::Data(Bytes::from_static(b"c")));

        let third = stream.next().await.unwrap().unwrap();
        assert_eq!(third, StreamMsg::Data(Bytes::from_static(b"d")));

        assert!(stream.next().await.is_none());
    }

    struct AbortGuard {
        flag: Arc<AtomicBool>,
    }

    impl Drop for AbortGuard {
        fn drop(&mut self) {
            self.flag.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Clone)]
    struct DropSource {
        flag: Arc<AtomicBool>,
    }

    impl DropSource {
        fn new(flag: Arc<AtomicBool>) -> Self {
            Self { flag }
        }
    }

    impl EngineSource for DropSource {
        type Error = TestError;
        type Control = ();
        type Event = ();
        type State = Arc<AtomicBool>;

        fn init(
            &mut self,
            _params: StreamParams,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = Result<Self::State, StreamError<Self::Error>>>
                    + Send
                    + 'static,
            >,
        > {
            let flag = self.flag.clone();
            Box::pin(async move { Ok(flag) })
        }

        fn open_reader(
            &mut self,
            _state: &Self::State,
            _params: StreamParams,
        ) -> Result<EngineStream<Self::Control, Self::Event, Self::Error>, StreamError<Self::Error>>
        {
            Ok(stream::pending::<
                Result<StreamMsg<Self::Control, Self::Event>, StreamError<Self::Error>>,
            >()
            .boxed())
        }

        fn start_writer(
            &mut self,
            state: &Self::State,
            _params: StreamParams,
        ) -> Result<WriterTask<Self::Error>, StreamError<Self::Error>> {
            let flag = state.clone();
            Ok(tokio::spawn(async move {
                let _guard = AbortGuard { flag };
                loop {
                    sleep(Duration::from_secs(1)).await;
                }
            }))
        }

        fn seek_bytes(&mut self, _pos: u64) -> Result<(), StreamError<Self::Error>> {
            Ok(())
        }

        fn supports_seek(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn dropping_output_aborts_writer() {
        let flag = Arc::new(AtomicBool::new(false));
        let engine = Engine::new(DropSource::new(flag.clone()), StreamParams::default());
        let stream = engine.into_stream();
        drop(stream);

        for _ in 0..10 {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert!(flag.load(Ordering::SeqCst));
    }
}
