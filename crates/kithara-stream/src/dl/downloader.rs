//! [`Downloader`] — unified download orchestrator implementation.

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures::{Stream, StreamExt, stream::SelectAll};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{
    CancelGroup, Mutex,
    time::Duration,
    tokio,
    tokio::{sync::mpsc, task},
};
use tokio_util::sync::CancellationToken;

use super::{
    cmd::{FetchCmd, FetchMethod, Priority},
    peer::{InternalCmd, Peer, PeerHandle, ResponseTarget},
    response::{BodyStream, FetchResponse},
};

/// Unified downloader — sole HTTP client owner and fetch orchestrator.
///
/// Created once at the application level, then shared (via [`Clone`]) across
/// protocol configs. Owns the [`HttpClient`] and the runtime handle.
/// Protocols obtain a [`PeerHandle`] via [`register`](Self::register) and
/// issue fetches through [`PeerHandle::execute`].
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<DownloaderInner>,
}

impl std::fmt::Debug for Downloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloader").finish_non_exhaustive()
    }
}

/// Peer registration entry sent to the download loop.
pub(super) struct RegisteredPeerEntry {
    pub(super) peer: Arc<dyn Peer>,
    pub(super) cmd_rx: mpsc::Receiver<InternalCmd>,
}

/// Merged stream: polls both the execute/batch channel AND
/// `Peer::poll_next` for a single peer, yielding `InternalCmd`s
/// from either source.
struct PeerStream {
    rx: mpsc::Receiver<InternalCmd>,
    peer: Arc<dyn Peer>,
    peer_cancel: CancellationToken,
    peer_done: bool,
    buffer: VecDeque<InternalCmd>,
    in_flight: Arc<AtomicUsize>,
    waker: Arc<futures::task::AtomicWaker>,
}

impl PeerStream {
    fn batch_to_buffer(&mut self, batch: Vec<FetchCmd>) {
        let priority = self.peer.priority();
        for mut cmd in batch {
            let epoch_cancel = cmd.cancel.take();
            let cancel = match epoch_cancel {
                Some(epoch) => CancelGroup::new(vec![self.peer_cancel.clone(), epoch]),
                None => CancelGroup::new(vec![self.peer_cancel.child_token()]),
            };
            self.in_flight.fetch_add(1, Ordering::Relaxed);
            self.buffer.push_back(InternalCmd {
                cmd,
                cancel,
                priority,
                response: ResponseTarget::Streaming {
                    in_flight: Arc::clone(&self.in_flight),
                    waker: Arc::clone(&self.waker),
                },
            });
        }
    }
}

impl Stream for PeerStream {
    type Item = InternalCmd;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<InternalCmd>> {
        let this = self.get_mut();
        this.waker.register(cx.waker());

        // 1. Always check the execute/batch channel first (High priority).
        if let Poll::Ready(Some(cmd)) = this.rx.poll_recv(cx) {
            return Poll::Ready(Some(cmd));
        }

        // 2. Drain buffered commands, skipping cancelled (stale epoch).
        while let Some(cmd) = this.buffer.pop_front() {
            if !cmd.cancel.is_cancelled() {
                return Poll::Ready(Some(cmd));
            }
            deliver_cancelled(cmd.response, cmd.cmd);
        }

        // 3. Poll the Peer's proactive stream (if still active and under limit).
        let at_capacity = this.in_flight.load(Ordering::Relaxed) >= this.peer.max_concurrent();
        if !this.peer_done && !at_capacity {
            match this.peer.poll_next(cx) {
                Poll::Ready(Some(batch)) => {
                    this.batch_to_buffer(batch);
                    if let Some(cmd) = this.buffer.pop_front() {
                        return Poll::Ready(Some(cmd));
                    }
                }
                Poll::Ready(None) => {
                    this.peer_done = true;
                }
                Poll::Pending => {}
            }
        }

        Poll::Pending
    }
}

/// Shared inner state for the downloader.
///
/// Both [`Downloader`] and [`PeerHandle`] hold an `Arc` to this; cloning
/// either is just an Arc bump.
pub(super) struct DownloaderInner {
    pub(super) client: HttpClient,
    pub(super) cancel: CancellationToken,
    pub(super) chunk_timeout: Duration,
    pub(super) runtime: Option<tokio::runtime::Handle>,
    /// Sender for registering new peers (cold path).
    pub(super) register_tx: mpsc::UnboundedSender<RegisteredPeerEntry>,
    /// Receiver — taken once by [`ensure_spawned`](Downloader::ensure_spawned).
    pub(super) register_rx: Mutex<Option<mpsc::UnboundedReceiver<RegisteredPeerEntry>>>,
}

impl Downloader {
    /// Create a new downloader from configuration.
    ///
    /// Constructs the internal `HttpClient` from the supplied network options.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let chunk_timeout = config.net.request_timeout;
        let runtime = config.runtime;
        Self {
            inner: Arc::new(DownloaderInner {
                client: HttpClient::new(config.net),
                cancel: config.cancel,
                chunk_timeout,
                runtime,
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
            }),
        }
    }

    /// Register a peer and return its [`PeerHandle`].
    ///
    /// Creates a per-peer cancel token (child of the downloader cancel)
    /// and a bounded command channel. The download loop is lazily spawned
    /// on first call.
    pub fn register(&self, peer: Arc<dyn Peer>) -> PeerHandle {
        self.ensure_spawned();
        let cancel = self.inner.cancel.child_token();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let entry = RegisteredPeerEntry { peer, cmd_rx };
        let _ = self.inner.register_tx.send(entry);
        PeerHandle::new(Arc::clone(&self.inner), cancel, cmd_tx)
    }

    /// Ensure the download loop is running (lazy spawn on first register
    /// in an async-capable context).
    ///
    /// If neither an explicit runtime handle nor an ambient tokio runtime
    /// is available (e.g. a synchronous unit test that only wants a
    /// `PeerHandle` for non-network purposes), the spawn is deferred —
    /// `register_rx` stays in place and the next call from an async
    /// context will pick it up.
    fn ensure_spawned(&self) {
        let handle = self
            .inner
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok());
        let Some(handle) = handle else {
            return;
        };
        let Some(rx) = self.inner.register_rx.lock_sync().take() else {
            return;
        };
        let this = self.clone();
        handle.spawn(async move { this.run(rx).await });
    }

    /// Download loop.
    ///
    /// Polls per-peer command channels via `SelectAll`, establishes
    /// HTTP connections, and sends responses back through oneshot
    /// channels. Each fetch runs as an independent task.
    ///
    /// No `hang_watchdog` — this is an event-driven loop with expected
    /// idle periods between command bursts. Protection comes from
    /// cancel tokens and channel timeouts.
    async fn run(&self, mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let mut streams: SelectAll<PeerStream> = SelectAll::new();
        let mut has_peers = false;
        let mut throttled: Vec<InternalCmd> = Vec::new();

        loop {
            // Retry throttled commands whose peer priority is no longer Low.
            let prev = std::mem::take(&mut throttled);
            for cmd in prev {
                if cmd.cancel.is_cancelled() {
                    deliver_cancelled(cmd.response, cmd.cmd);
                    continue;
                }
                if cmd.priority == Priority::Low {
                    throttled.push(cmd);
                    continue;
                }
                spawn_fetch(&self.inner, cmd);
            }

            while let Ok(entry) = register_rx.try_recv() {
                streams.push(PeerStream {
                    rx: entry.cmd_rx,
                    peer: entry.peer,
                    peer_cancel: self.inner.cancel.child_token(),
                    peer_done: false,
                    buffer: VecDeque::new(),
                    in_flight: Arc::new(AtomicUsize::new(0)),
                    waker: Arc::new(futures::task::AtomicWaker::new()),
                });
                has_peers = true;
            }

            if !has_peers {
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    entry = register_rx.recv() => match entry {
                        Some(e) => {
                            streams.push(PeerStream {
                                rx: e.cmd_rx,
                                peer: e.peer,
                                peer_cancel: self.inner.cancel.child_token(),
                                peer_done: false,
                                buffer: VecDeque::new(),
                                in_flight: Arc::new(AtomicUsize::new(0)),
                                waker: Arc::new(futures::task::AtomicWaker::new()),
                            });
                            has_peers = true;
                            continue;
                        }
                        None => return,
                    },
                }
            } else {
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    entry = register_rx.recv() => {
                        if let Some(e) = entry {
                            streams.push(PeerStream {
                                rx: e.cmd_rx,
                                peer: e.peer,
                                peer_cancel: self.inner.cancel.child_token(),
                                peer_done: false,
                                buffer: VecDeque::new(),
                                in_flight: Arc::new(AtomicUsize::new(0)),
                                waker: Arc::new(futures::task::AtomicWaker::new()),
                            });
                        }
                        continue;
                    },
                    cmd = streams.next() => {
                        if let Some(internal) = cmd {
                            if internal.cancel.is_cancelled() {
                                deliver_cancelled(internal.response, internal.cmd);
                                continue;
                            }
                            // Throttle: Low-priority streaming commands → buffer
                            if internal.priority == Priority::Low {
                                throttled.push(internal);
                                continue;
                            }
                            spawn_fetch(&self.inner, internal);
                            // Yield so spawned fetch tasks get scheduled.
                            task::yield_now().await;
                        } else {
                            has_peers = false;
                            continue;
                        }
                    },
                }
            }
        }
    }
}

/// Establish an HTTP connection and return a [`FetchResponse`].
///
/// Connects to the remote server, wraps the body in a [`BodyStream`]
/// with per-chunk cancel + timeout, and returns headers + body.
async fn establish(
    client: &HttpClient,
    chunk_timeout: Duration,
    cancel: &CancelGroup,
    cmd: FetchCmd,
) -> Result<FetchResponse, NetError> {
    let FetchCmd {
        method,
        url,
        range,
        headers,
        ..
    } = cmd;

    if method == FetchMethod::Head {
        let resp_headers = tokio::select! {
            () = cancel.cancelled() => return Err(NetError::Cancelled),
            r = client.head(url, headers) => r?,
        };
        return Ok(FetchResponse {
            headers: resp_headers,
            body: BodyStream::empty(),
        });
    }

    let byte_stream = tokio::select! {
        () = cancel.cancelled() => return Err(NetError::Cancelled),
        r = async {
            match range {
                Some(range) => client.get_range(url, range, headers).await,
                None => client.stream(url, headers).await,
            }
        } => r?,
    };

    let resp_headers = byte_stream.headers.clone();
    let body = BodyStream::from_http(byte_stream, cancel.clone(), chunk_timeout);
    Ok(FetchResponse {
        headers: resp_headers,
        body,
    })
}

/// Spawn an HTTP fetch task for one command.
fn spawn_fetch(inner: &Arc<DownloaderInner>, internal: InternalCmd) {
    let client = inner.client.clone();
    let timeout = inner.chunk_timeout;
    // Extract writer/on_complete before establish consumes the FetchCmd.
    let mut cmd = internal.cmd;
    let writer = cmd.writer.take();
    let on_complete_cb = cmd.on_complete.take();
    task::spawn(async move {
        let result = establish(&client, timeout, &internal.cancel, cmd).await;
        deliver(internal.response, result, writer, on_complete_cb).await;
    });
}

/// Route a fetch result to its target.
async fn deliver(
    target: ResponseTarget,
    result: Result<FetchResponse, NetError>,
    mut writer: Option<super::cmd::WriterFn>,
    on_complete_cb: Option<super::cmd::OnCompleteFn>,
) {
    match target {
        ResponseTarget::Channel(tx) => {
            let _ = tx.send(result);
        }
        ResponseTarget::Streaming { in_flight, waker } => {
            match result {
                Ok(resp) => {
                    if let Some(ref mut w) = writer {
                        let write_result = resp.body.write_all(|chunk| w(chunk)).await;
                        match write_result {
                            Ok(total) => {
                                if let Some(cb) = on_complete_cb {
                                    cb(total, None);
                                }
                            }
                            Err(ref e) => {
                                if let Some(cb) = on_complete_cb {
                                    cb(0, Some(e));
                                }
                            }
                        }
                    }
                }
                Err(ref e) => {
                    if let Some(cb) = on_complete_cb {
                        cb(0, Some(e));
                    }
                }
            }
            in_flight.fetch_sub(1, Ordering::Relaxed);
            waker.wake();
        }
    }
}

/// Route a cancellation to its target.
fn deliver_cancelled(target: ResponseTarget, mut cmd: FetchCmd) {
    let err = NetError::Cancelled;
    match target {
        ResponseTarget::Channel(tx) => {
            let _ = tx.send(Err(err));
        }
        ResponseTarget::Streaming { in_flight, waker } => {
            if let Some(cb) = cmd.on_complete.take() {
                cb(0, Some(&err));
            }
            in_flight.fetch_sub(1, Ordering::Relaxed);
            waker.wake();
        }
    }
}

impl Drop for DownloaderInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
