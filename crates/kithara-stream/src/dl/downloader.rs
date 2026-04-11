//! [`Downloader`] — unified download orchestrator implementation.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt, stream::SelectAll};
use kithara_bufpool::{BytePool, byte_pool};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{
    Mutex,
    time::{Duration, sleep},
    tokio,
    tokio::{sync::mpsc, task},
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{
    cmd::{FetchCmd, FetchMethod, FetchResult},
    handle::TrackHandle,
    peer::{InternalCmd, Peer, PeerHandle},
    response::{BodyStream, FetchResponse},
};

/// Backpressure pause between chunks when throttle returns `true`.
const THROTTLE_PAUSE: Duration = Duration::from_millis(10);

pub(super) type BoxStream = Pin<Box<dyn Stream<Item = FetchCmd> + Send>>;

/// Unified downloader — sole HTTP client owner and fetch orchestrator.
///
/// Created once at the application level, then shared (via [`Clone`]) across
/// protocol configs. Owns the [`HttpClient`], the runtime handle, and the
/// `Stream<Item = FetchCmd>` registration channel. The async loop polls all
/// registered streams cooperatively via `SelectAll`.
///
/// `Downloader` itself does not expose direct `execute*()` methods — those
/// live on [`TrackHandle`], which protocol code obtains via
/// [`track`](Self::track) (stream-less) or [`register`](Self::register)
/// (with a `Stream<FetchCmd>` source).
#[derive(Clone)]
pub struct Downloader {
    inner: Arc<DownloaderInner>,
}

impl std::fmt::Debug for Downloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloader").finish_non_exhaustive()
    }
}

/// Peer registration entry sent to the `run_v2` loop.
pub(super) struct RegisteredPeerEntry {
    pub(super) _peer: Arc<dyn Peer>,
    pub(super) cmd_rx: mpsc::Receiver<InternalCmd>,
}

/// Adapter: wrap `mpsc::Receiver<InternalCmd>` as a `Stream` for use
/// inside `SelectAll`.
struct PeerCmdStream {
    rx: mpsc::Receiver<InternalCmd>,
}

impl Stream for PeerCmdStream {
    type Item = InternalCmd;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<InternalCmd>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

/// Shared inner state for the downloader.
///
/// Both [`Downloader`] and [`TrackHandle`] hold an `Arc` to this; cloning
/// either is just an Arc bump. The state includes the HTTP client, the
/// chunk timeout, the pool, the runtime handle, and the registration
/// channel for new protocol streams.
pub(super) struct DownloaderInner {
    pub(super) client: HttpClient,
    pub(super) cancel: CancellationToken,
    pub(super) chunk_timeout: Duration,
    pub(super) pool: BytePool,
    pub(super) runtime: Option<tokio::runtime::Handle>,
    /// Sender for registering new protocol streams (cold path).
    pub(super) register_tx: mpsc::UnboundedSender<BoxStream>,
    /// Receiver — taken once by [`spawn`](Downloader::spawn).
    pub(super) register_rx: Mutex<Option<mpsc::UnboundedReceiver<BoxStream>>>,
    /// Sender for registering new peers (cold path).
    pub(super) register_peer_tx: mpsc::UnboundedSender<RegisteredPeerEntry>,
    /// Receiver — taken once by [`ensure_peer_loop_spawned`].
    pub(super) register_peer_rx: Mutex<Option<mpsc::UnboundedReceiver<RegisteredPeerEntry>>>,
}

impl Downloader {
    /// Create a new downloader from configuration.
    ///
    /// Constructs the internal `HttpClient` from the supplied network options.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (peer_tx, peer_rx) = mpsc::unbounded_channel();
        let chunk_timeout = config.net.request_timeout;
        let runtime = config.runtime;
        let pool = config.pool.unwrap_or_else(|| byte_pool().clone());
        Self {
            inner: Arc::new(DownloaderInner {
                client: HttpClient::new(config.net),
                cancel: config.cancel,
                chunk_timeout,
                pool,
                runtime,
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
                register_peer_tx: peer_tx,
                register_peer_rx: Mutex::new(Some(peer_rx)),
            }),
        }
    }

    /// Register a protocol stream as a new track and return its
    /// [`TrackHandle`].
    ///
    /// Legacy API — used by File protocol which yields
    /// `Stream<Item = FetchCmd>`. New protocols should use
    /// [`register`](Self::register) with the `Peer` trait instead.
    pub fn register_stream<S>(&self, stream: S) -> TrackHandle
    where
        S: Stream<Item = FetchCmd> + Send + Unpin + 'static,
    {
        self.ensure_spawned();
        let _ = self.inner.register_tx.send(Box::pin(stream));
        TrackHandle {
            pool: Arc::clone(&self.inner),
            state: super::handle::TrackInner::new(&self.inner.cancel),
        }
    }

    /// Create a new track without registering a protocol stream.
    ///
    /// Use this when a protocol drives all its fetches directly via
    /// [`TrackHandle::execute`] and has no `Stream<Item = FetchCmd>`
    /// source of its own. The returned handle has a fresh per-track
    /// cancellation token and behaves exactly like
    /// [`register`](Self::register) except that nothing is ever polled
    /// from a stream inside the downloader loop.
    ///
    /// The download loop is still spawned lazily on first call so that
    /// the per-track cancel is actually observed by [`execute`] callers.
    ///
    /// [`execute`]: TrackHandle::execute
    #[must_use]
    pub fn new_track(&self) -> TrackHandle {
        self.ensure_spawned();
        TrackHandle {
            pool: Arc::clone(&self.inner),
            state: super::handle::TrackInner::new(&self.inner.cancel),
        }
    }

    /// Ensure the download loop is running (lazy spawn on first register
    /// in an async-capable context).
    ///
    /// If neither an explicit runtime handle nor an ambient tokio runtime
    /// is available (e.g. a synchronous unit test that only wants a
    /// `TrackHandle` for non-network purposes), the spawn is deferred —
    /// `register_rx` stays in place and the next call from an async
    /// context will pick it up.
    fn ensure_spawned(&self) {
        // Resolve a runtime handle BEFORE taking `register_rx`, so a
        // failed spawn attempt does not consume the receiver.
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

    /// Register a peer and return its [`PeerHandle`].
    ///
    /// Creates a per-peer cancel token (child of the downloader cancel)
    /// and a bounded command channel. The peer loop is lazily spawned
    /// on first call.
    pub fn register(&self, peer: Arc<dyn Peer>) -> PeerHandle {
        self.ensure_peer_loop_spawned();
        let cancel = self.inner.cancel.child_token();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let entry = RegisteredPeerEntry {
            _peer: peer,
            cmd_rx,
        };
        let _ = self.inner.register_peer_tx.send(entry);
        PeerHandle::new(Arc::clone(&self.inner), cancel, cmd_tx)
    }

    /// Ensure the peer loop (`run_v2`) is running.
    fn ensure_peer_loop_spawned(&self) {
        let handle = self
            .inner
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok());
        let Some(handle) = handle else {
            return;
        };
        let Some(rx) = self.inner.register_peer_rx.lock_sync().take() else {
            return;
        };
        let this = self.clone();
        handle.spawn(async move { this.run_v2(rx).await });
    }

    /// Peer-based download loop.
    ///
    /// Polls per-peer command channels via `SelectAll`, establishes
    /// HTTP connections, and sends responses back through oneshot
    /// channels. Each fetch runs as an independent task.
    ///
    /// No `hang_watchdog` — this is an event-driven loop with expected
    /// idle periods between command bursts. Protection comes from
    /// cancel tokens and channel timeouts.
    async fn run_v2(&self, mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let mut cmd_streams: SelectAll<PeerCmdStream> = SelectAll::new();
        let mut has_peers = false;

        loop {
            while let Ok(entry) = register_rx.try_recv() {
                cmd_streams.push(PeerCmdStream { rx: entry.cmd_rx });
                has_peers = true;
            }

            if !has_peers {
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    entry = register_rx.recv() => match entry {
                        Some(e) => {
                            cmd_streams.push(PeerCmdStream { rx: e.cmd_rx });
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
                            cmd_streams.push(PeerCmdStream { rx: e.cmd_rx });
                        }
                        continue;
                    },
                    cmd = cmd_streams.next() => {
                        if let Some(internal) = cmd {
                            let client = self.inner.client.clone();
                            let timeout = self.inner.chunk_timeout;
                            task::spawn(async move {
                                let result = establish(
                                    &client, timeout, &internal.cancel, internal.cmd,
                                ).await;
                                let _ = internal.resp_tx.send(result);
                            });
                        } else {
                            has_peers = false;
                            continue;
                        }
                    },
                }
            }
        }
    }

    /// Core download loop.
    ///
    /// Picks up newly registered streams, waits for the next command from
    /// any stream via `SelectAll`, spawns its execution, and loops. Each
    /// fetch runs as an independent task so new commands (e.g. seek Range)
    /// are not blocked by in-flight downloads.
    #[kithara_hang_detector::hang_watchdog]
    async fn run(&self, mut rx: mpsc::UnboundedReceiver<BoxStream>) {
        let mut streams: SelectAll<BoxStream> = SelectAll::new();
        let mut ever_had_streams = false;

        loop {
            // Cold path: pick up newly registered streams.
            while let Ok(s) = rx.try_recv() {
                streams.push(s);
                ever_had_streams = true;
            }

            // Wait for next command (or registration, or cancel).
            let cmd = if !ever_had_streams {
                // No streams yet — wait for first registration.
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    stream = rx.recv() => match stream {
                        Some(s) => {
                            streams.push(s);
                            ever_had_streams = true;
                            hang_reset!();
                            continue;
                        }
                        None => return,
                    },
                }
            } else {
                tokio::select! {
                    biased;
                    () = self.inner.cancel.cancelled() => return,
                    stream = rx.recv() => {
                        if let Some(s) = stream {
                            streams.push(s);
                            ever_had_streams = true;
                        }
                        hang_reset!();
                        continue;
                    },
                    cmd = streams.next() => if let Some(c) = cmd { c } else {
                        debug!("all protocol streams completed, download loop exiting");
                        return;
                    },
                }
            };

            let client = self.inner.client.clone();
            let chunk_timeout = self.inner.chunk_timeout;
            let pool = self.inner.pool.clone();
            let cancel = self.inner.cancel.clone();
            task::spawn(async move {
                execute_one(&client, chunk_timeout, &pool, &cancel, cmd).await;
            });

            hang_tick!();
        }
    }
}

/// Execute a single fetch, calling `on_complete` with `&result` when done.
///
/// Convenience wrapper: runs the fetch via [`fetch_only`], then invokes
/// `on_complete` (if any) before returning the result.
pub(super) async fn execute_one(
    client: &HttpClient,
    chunk_timeout: Duration,
    pool: &BytePool,
    cancel: &CancellationToken,
    mut cmd: FetchCmd,
) -> FetchResult {
    let on_complete = cmd.on_complete.take();
    let result = fetch_only(client, chunk_timeout, pool, cancel, cmd).await;
    if let Some(cb) = on_complete {
        cb(&result);
    }
    result
}

/// Execute the fetch without calling `on_complete`.
///
/// Calls `on_connect`, streams body through `writer`, accumulates body
/// for [`FetchMethod::Get`], and returns the result. The caller is
/// responsible for invoking `on_complete` (if any) — this split enables
/// ordered `on_complete` delivery in batched execution where the caller
/// wants to fire callbacks in a deterministic order.
///
/// Honors `cancel` at three points: before the initial HEAD/GET request,
/// before each chunk wait in the body stream, and during throttle backoff.
/// Returns [`FetchResult::Err(NetError::Cancelled)`] when the token fires.
#[expect(clippy::cognitive_complexity)]
pub(super) async fn fetch_only(
    client: &HttpClient,
    chunk_timeout: Duration,
    pool: &BytePool,
    cancel: &CancellationToken,
    mut cmd: FetchCmd,
) -> FetchResult {
    let url = cmd.url.clone();

    // HEAD — headers only, no body.
    if cmd.method == FetchMethod::Head {
        return tokio::select! {
            () = cancel.cancelled() => FetchResult::Err(NetError::Cancelled),
            result = client.head(url.clone(), cmd.headers.take()) => match result {
                Ok(headers) => {
                    if let Some(on_connect) = cmd.on_connect.take() {
                        on_connect(&headers);
                    }
                    FetchResult::Ok {
                        bytes_written: 0,
                        headers,
                        body: None,
                    }
                }
                Err(e) => {
                    debug!(?url, "head failed: {e}");
                    FetchResult::Err(e)
                }
            },
        };
    }

    // GET or Stream — both fetch a body stream.
    let stream_result = tokio::select! {
        () = cancel.cancelled() => return FetchResult::Err(NetError::Cancelled),
        r = async {
            match cmd.range.take() {
                Some(range) => {
                    client
                        .get_range(url.clone(), range, cmd.headers.take())
                        .await
                }
                None => client.stream(url.clone(), cmd.headers.take()).await,
            }
        } => r,
    };

    let mut byte_stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            debug!(?url, "fetch failed: {e}");
            return FetchResult::Err(e);
        }
    };

    let response_headers = byte_stream.headers.clone();

    if let Some(on_connect) = cmd.on_connect.take() {
        on_connect(&response_headers);
    }

    // Get mode: accumulate body into pool buffer.
    let mut body_buf = if cmd.method == FetchMethod::Get {
        Some(pool.get())
    } else {
        None
    };

    let mut bytes_written: u64 = 0;

    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => return FetchResult::Err(NetError::Cancelled),
            c = byte_stream.next() => c,
            () = sleep(chunk_timeout) => None,
        };
        match chunk {
            Some(Ok(data)) => {
                if let Some(ref mut writer) = cmd.writer
                    && let Err(io_err) = writer(data.as_ref())
                {
                    debug!(?url, bytes_written, "writer error: {io_err}");
                    return FetchResult::Err(NetError::Http(io_err.to_string()));
                }
                if let Some(ref mut buf) = body_buf {
                    buf.extend_from_slice(data.as_ref());
                }
                bytes_written += data.len() as u64;

                // Backpressure: pause while download is too far ahead.
                if let Some(ref throttle) = cmd.throttle {
                    while throttle() {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                return FetchResult::Err(NetError::Cancelled);
                            }
                            () = sleep(THROTTLE_PAUSE) => {}
                        }
                    }
                }
            }
            Some(Err(net_err)) => {
                debug!(?url, bytes_written, "stream error: {net_err}");
                return FetchResult::Err(net_err);
            }
            None => break, // stream ended or chunk idle timeout
        }
    }

    debug!(?url, bytes_written, "fetch complete");
    FetchResult::Ok {
        bytes_written,
        headers: response_headers,
        body: body_buf.map(kithara_bufpool::PooledOwned::into_inner),
    }
}

/// Establish an HTTP connection and return a [`FetchResponse`].
///
/// Connects to the remote server, wraps the body in a [`BodyStream`]
/// with per-chunk cancel + timeout, and returns headers + body. The
/// callback fields on `cmd` (`writer`, `on_connect`, `on_complete`,
/// `throttle`) are ignored — the new API delivers data through the
/// body stream.
async fn establish(
    client: &HttpClient,
    chunk_timeout: Duration,
    cancel: &CancellationToken,
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

impl Drop for DownloaderInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
