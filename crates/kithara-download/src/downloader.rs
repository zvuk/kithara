use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use futures::task::AtomicWaker;
use kithara_abr::{Abr, AbrController, AbrPeerId};
use kithara_events::EventBus;
#[cfg(target_arch = "wasm32")]
use kithara_platform::thread::{keep_worker_alive, spawn};
use kithara_platform::{
    CancelScope, CancelToken,
    sync::{Arc, Mutex, Notify},
    time::Duration,
    tokio,
    tokio::{sync::mpsc, task},
};
use kithara_test_utils::kithara;

use super::{
    peer::{Peer, PeerHandle, PeerInner},
    registry::{FetchProgress, Registry},
};
use crate::RequestId;

/// Unified downloader — sole HTTP client owner and fetch orchestrator.
///
/// Created once at the application level, then shared (via [`Clone`]) across
/// protocol configs. Owns the [`kithara_net::HttpClient`] and the runtime handle.
/// Protocols obtain a [`PeerHandle`] via [`register`](Self::register) and
/// issue fetches through [`PeerHandle::execute`]. The download loop runs while
/// any clone or [`PeerHandle`] is alive, and stops when the last one drops.
#[derive(Clone, derive_more::Debug, kithara_config::ConfigOwner)]
#[config_owner(super::DownloaderConfig, inner.config)]
pub struct Downloader {
    #[debug(skip)]
    inner: Arc<DownloaderInner>,
    #[debug(skip)]
    _owner: Arc<LoopOwner>,
}

/// Held by every user-facing handle, [`Downloader`] clones and [`PeerHandle`]s
/// alike, and never by the download loop: when the last handle goes, the loop's
/// own subtree is cancelled, so a loop can never keep itself running.
struct LoopOwner {
    cancel: CancelToken,
}

impl Drop for LoopOwner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Peer registration entry sent to the download loop.
pub(super) struct RegisteredPeerEntry {
    /// ABR peer identifier assigned at registration. The Registry stamps
    /// every proactively-scheduled `InternalCmd` with this id so the
    /// Downloader can credit bandwidth samples to the right peer.
    pub(super) peer_id: AbrPeerId,
    /// Initial event destination supplied by the registering owner.
    pub(super) bus: Option<EventBus>,
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
    /// Completion edge consumed when queued work is blocked only by the global
    /// concurrency limit. Unlike the fetch waker, this retains one permit
    /// across the check-to-await window.
    pub(super) capacity_notify: Arc<Notify>,
    /// Global in-flight fetch counter. Limits total concurrent HTTP
    /// connections across all peers and command types.
    pub(super) inflight: Arc<AtomicUsize>,
    pub(super) cancel: CancelToken,
    pub(super) config: super::config::DownloaderConfig,
    /// Receiver — taken once by [`ensure_spawned`](Downloader::ensure_spawned).
    pub(super) register_rx: Mutex<Option<mpsc::UnboundedReceiver<RegisteredPeerEntry>>>,
    /// Sender for registering new peers (cold path).
    pub(super) register_tx: mpsc::UnboundedSender<RegisteredPeerEntry>,
    /// Monotonic source of [`crate::RequestId`]s assigned to
    /// every command this Downloader accepts. Starts at 1 (`NonZero`
    /// invariant); never wraps in practice (`u64`).
    next_request_id: AtomicU64,
}

impl DownloaderInner {
    /// Allocate a fresh [`crate::RequestId`].
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
    /// Retains `config.client` (a clone of the caller's [`kithara_net::HttpClient`])
    /// and the shared [`AbrController`] from `config.abr_settings`.
    ///
    /// Composed/standalone seam: a `Some` parent makes this token its child; `None` makes it its
    /// own root. The loop, peer scopes, and the shared ABR controller all derive from this token.
    #[must_use]
    pub fn new(mut config: super::DownloaderConfig) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancelScope::new(config.cancel.clone()).token();
        let mut abr_settings = std::mem::take(&mut config.abr_settings);
        abr_settings.cancel = Some(cancel.clone());
        let abr = AbrController::new(abr_settings);
        Self {
            _owner: Arc::new(LoopOwner {
                cancel: cancel.clone(),
            }),
            inner: Arc::new(DownloaderInner {
                config,
                abr,
                cancel,
                inflight: Arc::new(AtomicUsize::new(0)),
                fetch_waker: Arc::new(AtomicWaker::new()),
                capacity_notify: Arc::new(Notify::default()),
                register_tx: tx,
                register_rx: Mutex::new(Some(rx)),
                next_request_id: AtomicU64::new(1),
            }),
        }
    }

    /// Ensure the download loop is running (lazy spawn on first register
    /// in an async-capable context).
    fn ensure_spawned(&self) {
        let Some(rx) = self.inner.register_rx.lock().take() else {
            return;
        };
        Self::spawn_run(Arc::clone(&self.inner), rx);
    }

    pub(super) fn next_request_id(&self) -> RequestId {
        self.inner.next_request_id()
    }

    /// Register a peer and return its [`PeerHandle`].
    ///
    /// Double-registers the peer: fetch channel through the download loop
    /// and ABR state through the shared controller. The returned handle's
    /// `Drop` unregisters both. The initial `bus` is installed before the
    /// download loop can poll the peer.
    pub fn register(&self, peer: Arc<dyn Peer>, bus: Option<EventBus>) -> PeerHandle {
        self.ensure_spawned();
        let cancel = CancelScope::new(Some(self.inner.cancel.clone()));
        let cancel_token = cancel.token();
        let (cmd_tx, cmd_rx) = mpsc::channel(self.inner.config.peer_cmd_channel_capacity);

        let abr_peer: Arc<dyn Abr> = Arc::clone(&peer) as Arc<dyn Abr>;
        let mut abr_handle = self.inner.abr.register(&abr_peer);
        if let Some(bus) = &bus {
            abr_handle = abr_handle.with_bus(bus.clone());
        }
        let peer_id = abr_handle.peer_id();

        let entry = RegisteredPeerEntry {
            peer,
            cmd_rx,
            peer_id,
            cancel: cancel_token,
            bus: bus.clone(),
        };
        self.inner.register_tx.send(entry).ok();
        PeerHandle::new(
            PeerInner::builder()
                .downloader(self.clone())
                .cancel(cancel)
                .cmd_tx(cmd_tx)
                .maybe_bus(bus)
                .abr(abr_handle)
                .build(),
        )
    }

    /// Drive registry priorities and batches without dropping `process()` at a competing select arm:
    /// unspawned commands own completion callbacks that must settle their claims.
    /// Watchdog progress resets on `Advanced`, ignores legitimate `Idle`, and counts only `Stalled`.
    #[kithara::hang_watchdog(timeout = Self::HANG_TIMEOUT)]
    async fn run(
        inner: Arc<DownloaderInner>,
        mut register_rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>,
    ) {
        let mut registry = Registry::default();

        loop {
            let progress = tokio::select! {
                biased;
                () = inner.cancel.cancelled() => return,
                p = registry.tick(&inner, &mut register_rx) => p,
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
    fn spawn_run(inner: Arc<DownloaderInner>, rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        let handle = inner.config.runtime.clone();
        task::spawn_on(&handle, Self::run(inner, rx));
    }

    /// Runs the download loop on a dedicated Web Worker: the decoder blocks the engine worker in
    /// `wait_range` via `Atomics.wait`, so a `spawn_local` loop on that same worker would never be
    /// polled and its fetches would never complete the bytes the blocking read waits for.
    #[cfg(target_arch = "wasm32")]
    fn spawn_run(inner: Arc<DownloaderInner>, rx: mpsc::UnboundedReceiver<RegisteredPeerEntry>) {
        spawn(move || {
            keep_worker_alive();
            drop(task::spawn(Self::run(inner, rx)));
        });
    }
}
