use std::task::{Context, Poll};

use bytes::Bytes;
use futures::future::join_all;
use kithara_abr::{Abr, AbrHandle, AbrPeerId};
use kithara_events::EventBus;
use kithara_net::{Headers, NetError};
use kithara_platform::{
    CancelGroup, CancelScope, CancelToken,
    sync::Arc,
    time::Instant,
    tokio::sync::{mpsc, oneshot},
};
use kithara_test_utils::kithara;

use super::{cmd::FetchCmd, downloader::Downloader, response::FetchResponse};
use crate::RequestPriority;

/// Channel-path payload: a fully buffered response (headers + body bytes).
///
/// `Send` on every target, unlike a streaming [`FetchResponse`] whose
/// wasm body is `!Send`. The download worker collects the body before
/// replying so the engine worker — possibly a different Web Worker — only
/// ever receives `Send` data. [`PeerHandle::execute`]/[`batch`] re-wrap it
/// into a [`FetchResponse`] so callers see the same API.
pub(super) type CollectedResponse = (Headers, Bytes);

/// Re-wrap a collected channel-path payload into a [`FetchResponse`] so
/// `execute`/`batch` callers keep the streaming API. The body is a single
/// buffered chunk.
fn collected_into_response((headers, bytes): CollectedResponse) -> FetchResponse {
    FetchResponse {
        headers,
        body: bytes.into(),
    }
}

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
pub trait Peer: Abr {
    /// Yield the next batch of commands for the Downloader to execute.
    ///
    /// Returns `Ready(Some(batch))` with one or more self-contained
    /// [`FetchCmd`]s. Each command carries its own `writer` and
    /// `on_complete` closures — the Downloader calls them directly.
    ///
    /// `budget` bounds new commands by the downloader's available capacity.
    /// At zero, reconcile reader demand and cancellation without issuing work.
    ///
    /// Returns `Ready(None)` when the peer has no more work (stream
    /// ended). Returns `Pending` when waiting (throttle, idle).
    fn poll_next(&self, _cx: &mut Context<'_>, _budget: usize) -> Poll<Option<Vec<FetchCmd>>> {
        Poll::Ready(None)
    }

    /// Peer-level priority. `High` = active playback track.
    fn priority(&self) -> RequestPriority {
        RequestPriority::Low
    }
}

/// How the Downloader delivers the response for a command.
pub(super) enum ResponseTarget {
    /// Imperative path: send via oneshot (`execute` / `batch`). Carries a
    /// fully collected [`CollectedResponse`] (not a stream) so the payload
    /// stays `Send` across the download↔engine worker boundary on wasm.
    Channel(oneshot::Sender<Result<CollectedResponse, NetError>>),
    /// Streaming path: per-command `writer`/`on_complete` in [`FetchCmd`].
    Streaming,
}

/// Pair of an [`InternalCmd`] and the **peer-level** cancel token used
/// to discriminate `CancelReason` later in `deliver`.
///
/// The peer-level token lives in [`PeerInner::cancel`] (and the
/// Registry's `PeerEntry::peer_cancel`). Carrying a clone alongside
/// the cmd through the slot queue lets the spawned fetch task report
/// `CancelReason::PeerCancel` without holding a reference back into
/// the Registry.
pub(super) struct SlotEntry {
    pub(super) peer_cancel: CancelToken,
    pub(super) cmd: InternalCmd,
}

/// Per-peer command sent through the channel to the downloader loop.
pub(super) struct InternalCmd {
    /// ABR peer identifier for bandwidth accounting after fetch completes.
    pub(super) peer_id: AbrPeerId,
    pub(super) cancel: CancelGroup,
    pub(super) cmd: FetchCmd,
    /// Wall-clock instant the cmd was placed into a priority slot.
    /// Used to compute `RequestStarted::wait_in_queue` later in the
    /// pipeline.
    pub(super) enqueued_at: Instant,
    /// Bus of the peer that issued this command. Downloader publishes
    /// per-fetch `DownloaderEvent`s here.
    pub(super) bus: Option<EventBus>,
    /// Arena index of the owning peer. `None` when sent from `PeerHandle`
    /// (filled in by Registry on receipt).
    pub(super) peer: Option<thunderdome::Index>,
    /// Stable id allocated by the Downloader on enqueue. Carried in
    /// every `DownloaderEvent` for this fetch's lifecycle so subscribers
    /// can correlate Enqueued → Started → Completed/Failed/Cancelled.
    pub(super) request_id: crate::RequestId,
    pub(super) priority: RequestPriority,
    pub(super) response: ResponseTarget,
}

impl InternalCmd {
    /// Scheduling priority as of *now*: the stamped priority, escalated to
    /// `High` while the live demand probe reports a blocked reader.
    pub(super) fn effective_priority(&self) -> RequestPriority {
        if self.cmd.is_demanded() {
            return RequestPriority::High;
        }
        self.priority
    }
}

/// Shared per-peer state. Cancel fires when the last clone is dropped.
#[derive(bon::Builder)]
pub(super) struct PeerInner {
    /// ABR side of the double registration. Keeps the peer registered
    /// with the shared `AbrController` until the last `PeerHandle` drops
    /// (the handle's `Drop` calls `controller.unregister`).
    abr: AbrHandle,
    /// Keeps the downloader, and with it the download loop, alive for this
    /// peer's lifetime.
    downloader: Downloader,
    /// Initial event destination for imperative and proactive fetches.
    bus: Option<EventBus>,
    /// Canonical lifetime owner for this peer's fetches and ABR retries.
    cancel: CancelScope,
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
#[derive(Clone, derive_more::Debug)]
pub struct PeerHandle {
    #[debug(skip)]
    inner: Arc<PeerInner>,
}

impl PeerHandle {
    pub(super) fn new(inner: PeerInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    /// ABR-side handle attached at registration.
    #[must_use]
    pub fn abr(&self) -> &AbrHandle {
        &self.inner.abr
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
        let mut receivers: Vec<Option<oneshot::Receiver<Result<CollectedResponse, NetError>>>> =
            Vec::with_capacity(cmds.len());
        let bus = self.bus();
        let peer_id = self.inner.abr.peer_id();

        for cmd in cmds {
            let (internal, resp_rx) = self.make_imperative(cmd, bus.clone(), peer_id);
            if self.inner.cmd_tx.send(internal).await.is_err() {
                receivers.push(None);
                continue;
            }
            receivers.push(Some(resp_rx));
        }

        join_all(receivers.into_iter().map(|rx| async move {
            match rx {
                Some(resp_rx) => resp_rx
                    .await
                    .unwrap_or(Err(NetError::Cancelled))
                    .map(collected_into_response),
                None => Err(NetError::Cancelled),
            }
        }))
        .await
    }

    /// Event destination supplied at registration, if any.
    #[must_use]
    pub fn bus(&self) -> Option<EventBus> {
        self.inner.bus.clone()
    }

    /// Peer-level cancellation token.
    ///
    /// Cancelling this token aborts all in-flight fetches for this
    /// peer. The cancel also fires automatically when the last clone
    /// of this handle is dropped.
    #[must_use]
    pub fn cancel(&self) -> CancelToken {
        self.inner.cancel.token()
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
        let (internal, resp_rx) = self.make_imperative(cmd, self.bus(), self.inner.abr.peer_id());
        self.inner
            .cmd_tx
            .send(internal)
            .await
            .map_err(|_| NetError::Cancelled)?;
        resp_rx
            .await
            .map_err(|_| NetError::Cancelled)?
            .map(collected_into_response)
    }

    /// Build a High-priority imperative `InternalCmd` paired with its
    /// response receiver. Shared by [`Self::execute`] and [`Self::batch`].
    /// The enqueue stamp is read on the clock the batch measures its queue
    /// wait on.
    #[kithara::flash(true)]
    fn make_imperative(
        &self,
        cmd: FetchCmd,
        bus: Option<EventBus>,
        peer_id: AbrPeerId,
    ) -> (
        InternalCmd,
        oneshot::Receiver<Result<CollectedResponse, NetError>>,
    ) {
        let cancel = CancelGroup::new(vec![self.inner.cancel.token().child()]);
        let (resp_tx, resp_rx) = oneshot::channel();
        let request_id = self.inner.downloader.next_request_id();
        let enqueued_at = Instant::now();
        let internal = InternalCmd {
            cmd,
            cancel,
            bus,
            peer_id,
            request_id,
            enqueued_at,
            priority: RequestPriority::High,
            response: ResponseTarget::Channel(resp_tx),
            peer: None,
        };
        (internal, resp_rx)
    }

    /// ABR peer identifier for this handle.
    #[must_use]
    pub fn peer_id(&self) -> AbrPeerId {
        self.inner.abr.peer_id()
    }
}
