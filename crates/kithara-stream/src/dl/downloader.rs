//! [`Downloader`] — unified download orchestrator implementation.

use std::sync::{Arc, atomic::AtomicUsize};

use futures::task::AtomicWaker;
use kithara_events::EventBus;
use kithara_net::HttpClient;
use kithara_platform::{Mutex, RwLock, time::Duration, tokio, tokio::sync::mpsc};
use tokio_util::sync::CancellationToken;

use super::{
    peer::{Peer, PeerHandle},
    registry::Registry,
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
}

impl Downloader {
    /// Create a new downloader from configuration.
    ///
    /// Constructs the internal `HttpClient` from the supplied network options.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let chunk_timeout = config.net.request_timeout;
        let soft_timeout = config.soft_timeout;
        let runtime = config.runtime;
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
        let (cmd_tx, cmd_rx) = mpsc::channel(PEER_CMD_CHANNEL_CAPACITY);
        let bus: Arc<RwLock<Option<EventBus>>> = Arc::new(RwLock::new(None));
        let entry = RegisteredPeerEntry {
            peer,
            cmd_rx,
            cancel: cancel.clone(),
            bus: Arc::clone(&bus),
        };
        let _ = self.inner.register_tx.send(entry);
        PeerHandle::new(Arc::clone(&self.inner), cancel, cmd_tx, bus)
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
    async fn run(&self, mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let mut registry = Registry::new();

        loop {
            tokio::select! {
                biased;
                () = self.inner.cancel.cancelled() => return,
                () = registry.tick(&self.inner, &mut register_rx) => {},
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
