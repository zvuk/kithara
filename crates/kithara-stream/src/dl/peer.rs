//! Peer trait + per-peer handle for the channel-based downloader API.

use std::sync::Arc;

use kithara_net::NetError;
use kithara_platform::tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{cmd::FetchCmd, response::FetchResponse};

/// Protocol-agnostic contract for download orchestration.
///
/// Protocols (HLS, File) implement this trait. The
/// [`Downloader`](super::Downloader) queries peers through this
/// interface without knowing domain specifics.
pub trait Peer: Send + Sync {
    /// Is this peer actively playing? Active peers get priority in the
    /// download queue.
    fn is_active(&self) -> bool;
}

/// Per-peer command sent through the channel to the downloader loop.
pub(super) struct InternalCmd {
    pub(super) cmd: FetchCmd,
    pub(super) cancel: CancellationToken,
    pub(super) resp_tx: oneshot::Sender<Result<FetchResponse, NetError>>,
}

/// Shared per-peer state. Cancel fires when the last clone is dropped.
struct PeerInner {
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
    pub(super) fn new(cancel: CancellationToken, cmd_tx: mpsc::Sender<InternalCmd>) -> Self {
        Self {
            inner: Arc::new(PeerInner { cancel, cmd_tx }),
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

    /// Submit a fetch command and await the response.
    ///
    /// Creates a per-command cancel token (child of this peer's
    /// cancel) and sends the command to the downloader loop. The
    /// response contains headers and a body stream.
    ///
    /// # Errors
    /// Returns [`NetError::Cancelled`] when the peer cancel fires,
    /// the downloader shuts down, or the HTTP request itself fails.
    pub async fn execute(&self, cmd: FetchCmd) -> Result<FetchResponse, NetError> {
        let cmd_cancel = self.inner.cancel.child_token();
        let (resp_tx, resp_rx) = oneshot::channel();
        let internal = InternalCmd {
            cmd,
            cancel: cmd_cancel,
            resp_tx,
        };
        self.inner
            .cmd_tx
            .send(internal)
            .await
            .map_err(|_| NetError::Cancelled)?;
        resp_rx.await.map_err(|_| NetError::Cancelled)?
    }
}
