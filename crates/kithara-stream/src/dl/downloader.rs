//! [`Downloader`] — unified download orchestrator implementation.

use std::sync::{Arc, atomic::AtomicUsize};

use futures::task::AtomicWaker;
use kithara_abr::{Abr, AbrController, AbrPeerId};
use kithara_events::EventBus;
use kithara_net::HttpClient;
use kithara_platform::{Mutex, RwLock, time::Duration, tokio, tokio::sync::mpsc};
use tokio_util::sync::CancellationToken;

use super::{
    peer::{Peer, PeerHandle},
    registry::{FetchProgress, Registry},
};

/// Capacity of the per-peer bounded command channel.
const PEER_CMD_CHANNEL_CAPACITY: usize = 32;

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
    pub(super) cmd_rx: mpsc::Receiver<super::peer::InternalCmd>,
    /// Handle's cancel token. Fires from `PeerInner::Drop` when the last
    /// `PeerHandle` clone is released, letting the [`Registry`] drop the
    /// peer entry (and its `Arc<dyn Peer>`).
    pub(super) cancel: CancellationToken,
    /// Shared bus reference. Written by
    /// [`PeerHandle::with_bus`](super::peer::PeerHandle::with_bus), read
    /// by the Registry when dispatching fetches so that
    /// `DownloaderEvent::LoadSlow` lands on the owning track's bus.
    pub(super) bus: Arc<RwLock<Option<EventBus>>>,
    /// ABR peer identifier assigned at registration. The Registry stamps
    /// every proactively-scheduled `InternalCmd` with this id so the
    /// Downloader can credit bandwidth samples to the right peer.
    pub(super) peer_id: AbrPeerId,
}

/// Shared inner state for the downloader.
///
/// Both [`Downloader`] and [`PeerHandle`] hold an `Arc` to this; cloning
/// either is just an Arc bump.
pub(super) struct DownloaderInner {
    pub(super) client: HttpClient,
    pub(super) cancel: CancellationToken,
    pub(super) chunk_timeout: Duration,
    pub(super) soft_timeout: Duration,
    pub(super) runtime: Option<tokio::runtime::Handle>,
    pub(super) max_concurrent: usize,
    pub(super) demand_throttle: Duration,
    /// Global in-flight fetch counter. Limits total concurrent HTTP
    /// connections across all peers and command types.
    pub(super) inflight: Arc<AtomicUsize>,
    /// Waker fired when a fetch task completes (inflight decremented).
    /// Wakes `poll_fn` in `Registry::tick` so it can spawn more work.
    pub(super) fetch_waker: Arc<AtomicWaker>,
    /// Sender for registering new peers (cold path).
    pub(super) register_tx: mpsc::UnboundedSender<RegisteredPeerEntry>,
    /// Receiver — taken once by [`ensure_spawned`](Downloader::ensure_spawned).
    pub(super) register_rx: Mutex<Option<mpsc::UnboundedReceiver<RegisteredPeerEntry>>>,
    /// Shared ABR controller. One per Downloader — peers register through
    /// `register()` and fetch-completion hooks call
    /// `controller.record_bandwidth(...)` automatically.
    pub(super) abr: Arc<AbrController>,
    /// Monotonic source of [`kithara_events::RequestId`]s assigned to
    /// every command this Downloader accepts. Starts at 1 (`NonZero`
    /// invariant); never wraps in practice (`u64`).
    next_request_id: std::sync::atomic::AtomicU64,
}

impl DownloaderInner {
    /// Allocate a fresh [`kithara_events::RequestId`].
    pub(super) fn next_request_id(&self) -> kithara_events::RequestId {
        let raw = self
            .next_request_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Safety: `next_request_id` is initialised at 1, fetch_add returns
        // the prior value and then increments — so `raw` is always >= 1
        // and never zero in practice. We also guard with a `max(1)` to
        // satisfy the NonZeroU64 invariant defensively.
        let nz = std::num::NonZeroU64::new(raw.max(1))
            .expect("next_request_id starts at 1, fetch_add never yields 0");
        kithara_events::RequestId::new(nz)
    }
}

impl Downloader {
    /// Create a new downloader from configuration.
    ///
    /// Constructs the internal `HttpClient` from the supplied network options
    /// and the shared [`AbrController`] from `config.abr_settings`.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        // Per-chunk inactivity timeout in the BodyStream wrapper —
        // semantically the Downloader-layer mirror of reqwest's
        // `inactivity_timeout`. Both are idle gates between consecutive
        // bytes of the response body; sharing one source of truth
        // (`inactivity_timeout`) keeps the two layers consistent and
        // independent from `total_timeout` (which caps the whole
        // request lifetime, not idleness).
        let chunk_timeout = config.net.inactivity_timeout;
        let soft_timeout = config.soft_timeout;
        let runtime = config.runtime;
        let abr = AbrController::new(config.abr_settings);
        Self {
            inner: Arc::new(DownloaderInner {
                client: HttpClient::new(config.net),
                cancel: config.cancel,
                chunk_timeout,
                soft_timeout,
                runtime,
                max_concurrent: config.max_concurrent,
                demand_throttle: config.demand_throttle,
                inflight: Arc::new(AtomicUsize::new(0)),
                fetch_waker: Arc::new(AtomicWaker::new()),
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
                abr,
                next_request_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    /// Shared ABR controller for this downloader.
    #[must_use]
    pub fn abr_controller(&self) -> &Arc<AbrController> {
        &self.inner.abr
    }

    /// Register a peer and return its [`PeerHandle`].
    ///
    /// Double-registers the peer: fetch channel through the download loop
    /// and ABR state through the shared controller. The returned handle's
    /// `Drop` unregisters both.
    pub fn register(&self, peer: Arc<dyn Peer>) -> PeerHandle {
        self.ensure_spawned();
        let cancel = self.inner.cancel.child_token();
        let (cmd_tx, cmd_rx) = mpsc::channel(PEER_CMD_CHANNEL_CAPACITY);
        let bus: Arc<RwLock<Option<EventBus>>> = Arc::new(RwLock::new(None));

        // `Peer: Abr` enables the upcast (stable trait-upcasting, Rust 1.86+).
        let abr_peer: Arc<dyn Abr> = Arc::clone(&peer) as Arc<dyn Abr>;
        let abr_handle = self.inner.abr.register(&abr_peer);
        let peer_id = abr_handle.peer_id();

        let entry = RegisteredPeerEntry {
            peer,
            cmd_rx,
            cancel: cancel.clone(),
            bus: Arc::clone(&bus),
            peer_id,
        };
        let _ = self.inner.register_tx.send(entry);
        PeerHandle::new(Arc::clone(&self.inner), cancel, cmd_tx, bus, abr_handle)
    }

    /// Ensure the download loop is running (lazy spawn on first register
    /// in an async-capable context).
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
    /// Drives the [`Registry`] which owns peers, routes commands through
    /// a 2×2 priority slot map, and executes batches via [`BatchGroup`].
    ///
    /// Registrations are polled inside `tick()` so that `process()` is
    /// never dropped mid-batch by a competing `select!` arm (cancellation-
    /// safety: dropping `process()` loses unspawned `FetchCmd`s whose
    /// `on_complete` callbacks will never fire).
    ///
    /// # Deadlock detection
    ///
    /// `tick` reports a [`FetchProgress`](super::registry::FetchProgress):
    /// - `Advanced` — something moved (cmd drained, peer yielded a batch,
    ///   urgent/demand batch processed, or inflight changed). Reset.
    /// - `Idle` — nothing to do: no queued cmds, no in-flight, peers
    ///   pending. Watchdog left as-is; legitimate quiet period.
    /// - `Stalled` — work exists (queued cmds or inflight > 0) but no
    ///   forward motion this tick. Tick the watchdog; N consecutive
    ///   stalls across the timeout window → panic.
    #[kithara_hang_detector::hang_watchdog(timeout = Duration::from_secs(60))]
    async fn run(&self, mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let mut registry = Registry::new();

        loop {
            let progress = tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                p = registry.tick(&self.inner, &mut register_rx) => p,
            };

            match progress {
                FetchProgress::Advanced => {
                    hang_reset!();
                }
                FetchProgress::Stalled => {
                    hang_tick!();
                }
                FetchProgress::Idle => {}
            }

            registry.reschedule();
        }
    }
}

impl Drop for DownloaderInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
