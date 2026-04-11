//! [`Downloader`] — unified download orchestrator implementation.

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{Stream, StreamExt, stream::SelectAll};
use kithara_net::{HttpClient, NetError};
use kithara_platform::{
    Mutex,
    time::Duration,
    tokio,
    tokio::{sync::mpsc, task},
};
use tokio_util::sync::CancellationToken;

use super::{
    cmd::{FetchCmd, FetchMethod},
    peer::{InternalCmd, Peer, PeerHandle},
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
        let entry = RegisteredPeerEntry {
            _peer: peer,
            cmd_rx,
        };
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
}

/// Establish an HTTP connection and return a [`FetchResponse`].
///
/// Connects to the remote server, wraps the body in a [`BodyStream`]
/// with per-chunk cancel + timeout, and returns headers + body.
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
