use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use derivative::Derivative;
use kithara_events::{AbrEvent, AbrMode, EventBus};
use kithara_platform::{
    Mutex, RwLock,
    time::{Duration, Instant},
};

use super::{peer::PeerEntry, throttle::EventThrottleCache};
use crate::{
    abr::Abr,
    estimator::{Estimator, ThroughputEstimator},
    handle::AbrHandle,
};

/// Opaque peer identifier assigned by the ABR controller on `register`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AbrPeerId(NonZeroU64);

impl AbrPeerId {
    /// Construct from a non-zero identifier.
    #[must_use]
    pub fn new(id: NonZeroU64) -> Self {
        Self(id)
    }
}

/// ABR controller settings.
#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct AbrSettings {
    /// Minimum interval between `AbrEvent::BandwidthEstimate` emits.
    #[derivative(Default(value = "Duration::from_secs(1)"))]
    pub bandwidth_emit_min_interval: Duration,
    /// Minimum absolute delta between `BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_delta: Duration,
    /// Minimum interval between `AbrEvent::BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_interval: Duration,
    /// Deadline for the incoherence watcher spawned after a variant switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub incoherence_deadline: Duration,
    /// Minimum buffer-ahead required before an up-switch is allowed.
    #[derivative(Default(value = "Duration::from_secs(10)"))]
    pub min_buffer_for_up_switch: Duration,
    /// Minimum interval between variant switches.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub min_switch_interval: Duration,
    /// Buffer-ahead at or below this threshold forces an urgent down-switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub urgent_downswitch_buffer: Duration,
    /// Global data-saver cap. Per-peer overrides live in `AbrState`.
    pub max_bandwidth_bps: Option<u64>,
    /// Minimum relative delta (0.0–1.0) between `BandwidthEstimate` emits.
    #[derivative(Default(value = "0.10"))]
    pub bandwidth_emit_min_delta_ratio: f64,
    /// Hysteresis ratio for down-switch.
    #[derivative(Default(value = "0.8"))]
    pub down_hysteresis_ratio: f64,
    /// Safety factor applied to the throughput estimate before comparing to
    /// candidate variants (e.g. `1.5` uses ~66% of the raw estimate).
    #[derivative(Default(value = "1.5"))]
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch (adjusted throughput must exceed
    /// candidate bandwidth by this factor).
    #[derivative(Default(value = "1.3"))]
    pub up_hysteresis_ratio: f64,
    /// Minimum download duration (ms) to record a bandwidth sample — fetches
    /// faster than this are ignored.
    #[derivative(Default(value = "10"))]
    pub min_throughput_record_ms: u128,
    /// Number of bytes that must be downloaded before ABR will switch.
    #[derivative(Default(value = "128 * 1024"))]
    pub warmup_min_bytes: u64,
}

/// Shared per-player ABR controller.
///
/// Holds the bandwidth estimator (one per controller) and a map of
/// registered peers. Constructed via [`AbrController::new`]; peers are
/// attached with [`AbrController::register`].
pub struct AbrController {
    pub(super) settings: AbrSettings,
    pub(super) estimator: Arc<dyn Estimator>,
    pub(super) self_weak: Weak<Self>,
    next_peer_id: AtomicU64,
    peers: Mutex<HashMap<AbrPeerId, Arc<PeerEntry>>>,
}

impl AbrController {
    /// Minimum delay between `AbrEvent::ThroughputSample` emits (fixed).
    pub(super) const MIN_THROUGHPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

    /// Create a new controller with the default [`ThroughputEstimator`].
    #[must_use]
    pub fn new(settings: AbrSettings) -> Arc<Self> {
        Self::with_estimator(settings, Arc::new(ThroughputEstimator::new()))
    }

    pub(super) fn allocate_peer_id(&self) -> AbrPeerId {
        let raw = self
            .next_peer_id
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        AbrPeerId::new(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| NonZeroU64::new(1).expect("1 is always non-zero")),
        )
    }

    /// Current shared bandwidth estimate.
    #[must_use]
    pub fn current_bandwidth_estimate_bps(&self) -> Option<u64> {
        self.estimator.estimate_bps()
    }

    pub(crate) fn on_locked(&self, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Locked);
        }
    }

    pub(crate) fn on_max_bandwidth_cap_changed(&self, peer_id: AbrPeerId, cap: Option<u64>) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::MaxBandwidthCapChanged { cap });
        }
        self.tick(peer_id, Instant::now());
    }

    pub(crate) fn on_mode_changed(&self, peer_id: AbrPeerId, mode: AbrMode) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::ModeChanged { mode });
        }
        self.tick(peer_id, Instant::now());
    }

    pub(crate) fn on_unlocked(&self, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Unlocked);
        }
        self.tick(peer_id, Instant::now());
    }

    pub(super) fn peer_entry(&self, id: AbrPeerId) -> Option<Arc<PeerEntry>> {
        self.peers.lock_sync().get(&id).cloned()
    }

    /// Register a peer. Returns an [`AbrHandle`] that the caller keeps alive
    /// for the peer's lifetime; the handle's `Drop` unregisters the peer.
    pub fn register(self: &Arc<Self>, peer: &Arc<dyn Abr>) -> AbrHandle {
        let id = self.allocate_peer_id();
        let state = peer.state();
        let peer_weak = Arc::downgrade(peer);
        let bus: Arc<RwLock<Option<EventBus>>> = Arc::new(RwLock::new(None));
        let entry = Arc::new(PeerEntry {
            peer_weak,
            bus: Arc::clone(&bus),
            variants_registered_published: AtomicBool::new(false),
            warmup_completed: AtomicBool::new(false),
            bytes_downloaded: AtomicU64::new(0),
            incoherence_cancel: Mutex::new(None),
            last_variant_switch: Mutex::new(None),
            throttle: Mutex::new(EventThrottleCache::default()),
            state: state.clone(),
        });
        self.peers.lock_sync().insert(id, entry);
        AbrHandle::new(Arc::clone(self), id, state, bus)
    }

    /// Settings snapshot.
    #[must_use]
    pub fn settings(&self) -> &AbrSettings {
        &self.settings
    }

    /// Called from [`AbrHandle::drop`].
    pub(crate) fn unregister(&self, id: AbrPeerId) {
        if let Some(entry) = self.peers.lock_sync().remove(&id)
            && let Some(token) = entry.incoherence_cancel.lock_sync().take()
        {
            token.cancel();
        }
    }

    /// Create a new controller with a custom estimator. Used in tests to
    /// inject a mock.
    #[must_use]
    pub fn with_estimator(settings: AbrSettings, estimator: Arc<dyn Estimator>) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            settings,
            estimator,
            self_weak: weak.clone(),
            next_peer_id: AtomicU64::new(0),
            peers: Mutex::new(HashMap::new()),
        })
    }
}
