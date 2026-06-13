use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use futures::task::AtomicWaker;
use kithara_abr::{Abr, AbrController, AbrPeerId};
use kithara_events::{EventBus, RequestId};
use kithara_net::HttpClient;
use kithara_platform::{
    CancelScope, CancelToken, Mutex, RwLock,
    time::Duration,
    tokio,
    tokio::{sync::mpsc, task},
};
use kithara_test_utils::kithara;

use super::{
    peer::{Peer, PeerHandle},
    registry::{FetchProgress, Registry},
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
    /// ABR peer identifier assigned at registration. The Registry stamps
    /// every proactively-scheduled `InternalCmd` with this id so the
    /// Downloader can credit bandwidth samples to the right peer.
    pub(super) peer_id: AbrPeerId,
    /// Shared bus reference. Written by
    /// [`PeerHandle::with_bus`](super::peer::PeerHandle::with_bus), read
    /// by the Registry when dispatching fetches so that
    /// `DownloaderEvent::LoadSlow` lands on the owning track's bus.
    pub(super) bus: Arc<RwLock<Option<EventBus>>>,
    pub(super) peer: Arc<dyn Peer>,
    /// Handle's cancel token. Fires from `PeerInner::Drop` when the last
    /// `PeerHandle` clone is released, letting the [`Registry`] drop the
    /// peer entry (and its `Arc<dyn Peer>`).
    pub(super) cancel: CancelToken,
    pub(super) cmd_rx: mpsc::Receiver<super::peer::InternalCmd>,
}

/// Shared inner state for the downloader.
///
/// Both [`Downloader`] and [`PeerHandle`] hold an `Arc` to this; cloning
/// either is just an Arc bump.
pub(super) struct DownloaderInner {
    /// Shared ABR controller. One per Downloader — peers register through
    /// `register()` and fetch-completion hooks call
    /// `controller.record_bandwidth(...)` automatically.
    pub(super) abr: Arc<AbrController>,
    /// Waker fired when a fetch task completes (inflight decremented).
    /// Wakes `poll_fn` in `Registry::tick` so it can spawn more work.
    pub(super) fetch_waker: Arc<AtomicWaker>,
    /// Global in-flight fetch counter. Limits total concurrent HTTP
    /// connections across all peers and command types.
    pub(super) inflight: Arc<AtomicUsize>,
    pub(super) cancel: CancelToken,
    pub(super) demand_throttle: Duration,
    pub(super) soft_timeout: Duration,
    pub(super) client: HttpClient,
    /// Receiver — taken once by [`ensure_spawned`](Downloader::ensure_spawned).
    pub(super) register_rx: Mutex<Option<mpsc::UnboundedReceiver<RegisteredPeerEntry>>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) runtime: Option<tokio::runtime::Handle>,
    /// Sender for registering new peers (cold path).
    pub(super) register_tx: mpsc::UnboundedSender<RegisteredPeerEntry>,
    pub(super) max_concurrent: usize,
    /// Monotonic source of [`kithara_events::RequestId`]s assigned to
    /// every command this Downloader accepts. Starts at 1 (`NonZero`
    /// invariant); never wraps in practice (`u64`).
    next_request_id: AtomicU64,
}

impl DownloaderInner {
    /// Allocate a fresh [`kithara_events::RequestId`].
    pub(super) fn next_request_id(&self) -> RequestId {
        let raw = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let nz = std::num::NonZeroU64::new(raw.max(1))
            .expect("BUG: next_request_id starts at 1; fetch_add never yields 0");
        RequestId::new(nz)
    }
}

impl Downloader {
    /// Hang-detector timeout for the downloader run loop. 60 s covers
    /// the worst legitimate quiet period (single peer waiting on a
    /// slow first byte) without delaying real-deadlock diagnostics.
    const HANG_TIMEOUT: Duration = Duration::from_secs(60);

    /// Create a new downloader from configuration.
    ///
    /// Adopts `config.client` (a clone of the caller's [`HttpClient`])
    /// and the shared [`AbrController`] from `config.abr_settings`.
    #[must_use]
    pub fn new(config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let soft_timeout = config.soft_timeout;
        #[cfg(not(target_arch = "wasm32"))]
        let runtime = config.runtime;
        // Composed/standalone seam: `Some` parent → child of it; `None` → own
        // root. The loop, peers, and ABR controller all derive from this token.
        let cancel = CancelScope::new(config.cancel).token();
        let abr = AbrController::new(config.abr_settings, cancel.child());
        Self {
            inner: Arc::new(DownloaderInner {
                soft_timeout,
                #[cfg(not(target_arch = "wasm32"))]
                runtime,
                abr,
                client: config.client,
                cancel,
                max_concurrent: config.max_concurrent,
                demand_throttle: config.demand_throttle,
                inflight: Arc::new(AtomicUsize::new(0)),
                fetch_waker: Arc::new(AtomicWaker::new()),
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
                next_request_id: AtomicU64::new(1),
            }),
        }
    }

    /// Ensure the download loop is running (lazy spawn on first register
    /// in an async-capable context).
    fn ensure_spawned(&self) {
        let Some(rx) = self.inner.register_rx.lock_sync().take() else {
            return;
        };
        let this = self.clone();
        Self::spawn_run(&self.inner, this, rx);
    }

    /// Register a peer and return its [`PeerHandle`].
    ///
    /// Double-registers the peer: fetch channel through the download loop
    /// and ABR state through the shared controller. The returned handle's
    /// `Drop` unregisters both.
    pub fn register(&self, peer: Arc<dyn Peer>) -> PeerHandle {
        /// Capacity of the per-peer bounded command channel.
        const PEER_CMD_CHANNEL_CAPACITY: usize = 32;
        self.ensure_spawned();
        let cancel = self.inner.cancel.child();
        let (cmd_tx, cmd_rx) = mpsc::channel(PEER_CMD_CHANNEL_CAPACITY);
        let bus: Arc<RwLock<Option<EventBus>>> = Arc::new(RwLock::new(None));

        let abr_peer: Arc<dyn Abr> = Arc::clone(&peer) as Arc<dyn Abr>;
        let abr_handle = self.inner.abr.register(&abr_peer);
        let peer_id = abr_handle.peer_id();

        let entry = RegisteredPeerEntry {
            peer,
            cmd_rx,
            peer_id,
            cancel: cancel.clone(),
            bus: Arc::clone(&bus),
        };
        self.inner.register_tx.send(entry).ok();
        PeerHandle::new(Arc::clone(&self.inner), cancel, cmd_tx, bus, abr_handle)
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
    #[kithara::hang_watchdog(timeout = Self::HANG_TIMEOUT)]
    async fn run(&self, mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let mut registry = Registry::default();

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

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_run(
        inner: &DownloaderInner,
        this: Self,
        rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>,
    ) {
        let Some(handle) = inner
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
        else {
            return;
        };
        task::spawn_on(&handle, async move { this.run(rx).await });
    }

    #[cfg(target_arch = "wasm32")]
    fn spawn_run(
        _inner: &DownloaderInner,
        this: Self,
        rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>,
    ) {
        // Run the download loop on a dedicated Web Worker (mirrors the
        // pre-`unified-Downloader` `Backend` model). The decoder blocks the
        // engine worker in `wait_range` (`Atomics.wait`); a `spawn_local`
        // loop on that same worker would never be polled, so its fetches
        // would never complete the bytes the blocking read waits for.
        // Storage is shared linear memory and its condvar notify crosses
        // workers via `Atomics.notify`, so the engine worker's blocked read
        // is woken once this worker commits bytes. `keep_worker_alive` keeps
        // the worker's event loop pumping for the page's lifetime.
        kithara_platform::thread::spawn(move || {
            kithara_platform::thread::keep_worker_alive();
            drop(task::spawn(async move {
                this.run(rx).await;
            }));
        });
    }
}

impl Drop for DownloaderInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
