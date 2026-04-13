//! Peer trait + per-peer handle for the channel-based downloader API.

use std::{
    sync::Arc,
    task::{Context, Poll},
};

use kithara_net::NetError;
use kithara_platform::tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    cmd::{FetchCmd, Priority},
    downloader::DownloaderInner,
    response::FetchResponse,
};

/// Protocol-agnostic contract for download orchestration.
///
/// Protocols (HLS, File) implement this trait. The
/// [`Downloader`](super::Downloader) queries peers through this
/// interface without knowing domain specifics.
///
/// All methods have defaults so that simple peers (File) only need
/// to exist — the Downloader drives everything through `execute()`.
/// Complex peers (HLS) override `poll_next` to let the Downloader
/// drive media segment downloads via per-command `writer`/`on_complete`
/// closures in [`FetchCmd`].
pub trait Peer: Send + Sync + 'static {
    /// Peer-level priority. `High` = active playback track.
    fn priority(&self) -> Priority {
        Priority::Low
    }

    /// Yield the next batch of commands for the Downloader to execute.
    ///
    /// Returns `Ready(Some(batch))` with one or more self-contained
    /// [`FetchCmd`]s. Each command carries its own `writer` and
    /// `on_complete` closures — the Downloader calls them directly.
    ///
    /// Returns `Ready(None)` when the peer has no more work (stream
    /// ended). Returns `Pending` when waiting (throttle, idle).
    fn poll_next(&self, _cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        Poll::Ready(None)
    }
}

/// How the Downloader delivers the response for a command.
pub(super) enum ResponseTarget {
    /// Imperative path: send via oneshot (`execute` / `batch`).
    Channel(oneshot::Sender<Result<FetchResponse, NetError>>),
    /// Streaming path: per-command `writer`/`on_complete` in [`FetchCmd`].
    Streaming,
}

/// Per-peer command sent through the channel to the downloader loop.
pub(super) struct InternalCmd {
    pub(super) cmd: FetchCmd,
    pub(super) cancel: kithara_platform::CancelGroup,
    pub(super) priority: Priority,
    pub(super) response: ResponseTarget,
    /// Arena index of the owning peer. `None` when sent from `PeerHandle`
    /// (filled in by Registry on receipt).
    pub(super) peer: Option<thunderdome::Index>,
}

/// Shared per-peer state. Cancel fires when the last clone is dropped.
struct PeerInner {
    /// Keeps `DownloaderInner` (`HttpClient`, cancel, runtime) alive
    /// for this peer's lifetime.
    _pool: Arc<DownloaderInner>,
    cancel: CancellationToken,
    cmd_tx: mpsc::Sender<InternalCmd>,
}

impl Drop for PeerInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Per-peer handle for submitting fetch commands and awaiting
/// responses.
///
/// Cheap to [`Clone`] (one Arc bump). When the last clone is dropped,
/// the peer-level cancel token fires, aborting all in-flight fetches
/// for this peer.
#[derive(Clone)]
pub struct PeerHandle {
    inner: Arc<PeerInner>,
}

impl std::fmt::Debug for PeerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerHandle").finish_non_exhaustive()
    }
}

impl PeerHandle {
    pub(super) fn new(
        pool: Arc<DownloaderInner>,
        cancel: CancellationToken,
        cmd_tx: mpsc::Sender<InternalCmd>,
    ) -> Self {
        Self {
            inner: Arc::new(PeerInner {
                _pool: pool,
                cancel,
                cmd_tx,
            }),
        }
    }

    /// Peer-level cancellation token.
    ///
    /// Cancelling this token aborts all in-flight fetches for this
    /// peer. The cancel also fires automatically when the last clone
    /// of this handle is dropped.
    #[must_use]
    pub fn cancel(&self) -> CancellationToken {
        self.inner.cancel.clone()
    }

    /// Submit a single fetch command and await the response.
    ///
    /// Always runs at `High` priority — imperative requests are
    /// latency-sensitive.
    ///
    /// # Errors
    /// Returns [`NetError::Cancelled`] when the peer cancel fires,
    /// the downloader shuts down, or the HTTP request itself fails.
    pub async fn execute(&self, cmd: FetchCmd) -> Result<FetchResponse, NetError> {
        let cancel = kithara_platform::CancelGroup::new(vec![self.inner.cancel.child_token()]);
        let (resp_tx, resp_rx) = oneshot::channel();
        let internal = InternalCmd {
            cmd,
            cancel,
            priority: Priority::High,
            response: ResponseTarget::Channel(resp_tx),
            peer: None,
        };
        self.inner
            .cmd_tx
            .send(internal)
            .await
            .map_err(|_| NetError::Cancelled)?;
        resp_rx.await.map_err(|_| NetError::Cancelled)?
    }

    /// Submit a batch of fetch commands and await all responses.
    ///
    /// Commands execute in parallel. Results are returned **in array
    /// order**, not completion order.
    ///
    /// # Errors
    /// Individual commands may fail independently. Each slot in the
    /// returned `Vec` contains its own `Result`.
    pub async fn batch(&self, cmds: Vec<FetchCmd>) -> Vec<Result<FetchResponse, NetError>> {
        let mut receivers: Vec<Option<oneshot::Receiver<Result<FetchResponse, NetError>>>> =
            Vec::with_capacity(cmds.len());

        for cmd in cmds {
            let cancel = kithara_platform::CancelGroup::new(vec![self.inner.cancel.child_token()]);
            let (resp_tx, resp_rx) = oneshot::channel();
            let internal = InternalCmd {
                cmd,
                cancel,
                priority: Priority::High,
                response: ResponseTarget::Channel(resp_tx),
                peer: None,
            };
            if self.inner.cmd_tx.send(internal).await.is_err() {
                receivers.push(None);
                continue;
            }
            receivers.push(Some(resp_rx));
        }

        // Await all responses concurrently, preserving array order.
        futures::future::join_all(receivers.into_iter().map(|rx| async move {
            match rx {
                Some(resp_rx) => resp_rx.await.unwrap_or(Err(NetError::Cancelled)),
                None => Err(NetError::Cancelled),
            }
        }))
        .await
    }
}
