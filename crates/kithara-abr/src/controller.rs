use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use derivative::Derivative;
use kithara_events::{AbrEvent, AbrMode, AbrReason, BandwidthSource, EventBus, VariantInfo};
use kithara_platform::{
    Mutex, RwLock,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::{
    abr::Abr,
    estimator::{Estimator, ThroughputEstimator},
    handle::AbrHandle,
    state::{AbrState, AbrView},
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

    /// Raw numeric value.
    #[must_use]
    pub fn get(self) -> NonZeroU64 {
        self.0
    }
}

/// ABR controller settings.
#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct AbrSettings {
    /// Number of bytes that must be downloaded before ABR will switch.
    #[derivative(Default(value = "128 * 1024"))]
    pub warmup_min_bytes: u64,
    /// Minimum buffer-ahead required before an up-switch is allowed.
    #[derivative(Default(value = "Duration::from_secs(10)"))]
    pub min_buffer_for_up_switch: Duration,
    /// Buffer-ahead at or below this threshold forces an urgent down-switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub urgent_downswitch_buffer: Duration,
    /// Minimum interval between variant switches.
    #[derivative(Default(value = "Duration::from_secs(30)"))]
    pub min_switch_interval: Duration,
    /// Safety factor applied to the throughput estimate before comparing to
    /// candidate variants (e.g. `1.5` uses ~66% of the raw estimate).
    #[derivative(Default(value = "1.5"))]
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch (adjusted throughput must exceed
    /// candidate bandwidth by this factor).
    #[derivative(Default(value = "1.3"))]
    pub up_hysteresis_ratio: f64,
    /// Hysteresis ratio for down-switch.
    #[derivative(Default(value = "0.8"))]
    pub down_hysteresis_ratio: f64,
    /// Minimum download duration (ms) to record a bandwidth sample — fetches
    /// faster than this are ignored.
    #[derivative(Default(value = "10"))]
    pub min_throughput_record_ms: u128,
    /// Global data-saver cap. Per-peer overrides live in `AbrState`.
    pub max_bandwidth_bps: Option<u64>,
    /// Deadline for the incoherence watcher spawned after a variant switch.
    #[derivative(Default(value = "Duration::from_secs(5)"))]
    pub incoherence_deadline: Duration,
    /// Minimum interval between `AbrEvent::BandwidthEstimate` emits.
    #[derivative(Default(value = "Duration::from_secs(1)"))]
    pub bandwidth_emit_min_interval: Duration,
    /// Minimum relative delta (0.0–1.0) between `BandwidthEstimate` emits.
    #[derivative(Default(value = "0.10"))]
    pub bandwidth_emit_min_delta_ratio: f64,
    /// Minimum interval between `AbrEvent::BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_interval: Duration,
    /// Minimum absolute delta between `BufferAhead` emits.
    #[derivative(Default(value = "Duration::from_millis(500)"))]
    pub buffer_emit_min_delta: Duration,
}

/// Shared per-player ABR controller.
///
/// Holds the bandwidth estimator (one per controller) and a map of
/// registered peers. Constructed via [`AbrController::new`]; peers are
/// attached with [`AbrController::register`].
pub struct AbrController {
    settings: AbrSettings,
    estimator: Arc<dyn Estimator>,
    peers: Mutex<HashMap<AbrPeerId, Arc<PeerEntry>>>,
    next_peer_id: AtomicU64,
    self_weak: Weak<Self>,
}

struct PeerEntry {
    peer_weak: Weak<dyn Abr>,
    state: Option<Arc<AbrState>>,
    bus: Arc<RwLock<Option<EventBus>>>,
    bytes_downloaded: AtomicU64,
    variants_registered_published: AtomicBool,
    warmup_completed: AtomicBool,
    last_variant_switch: Mutex<Option<(Instant, Duration)>>,
    incoherence_cancel: Mutex<Option<CancellationToken>>,
    throttle: Mutex<EventThrottleCache>,
}

impl PeerEntry {
    fn bus(&self) -> Option<EventBus> {
        self.bus.lock_sync_read().clone()
    }
}

#[derive(Default)]
struct EventThrottleCache {
    last_throughput_sample_at: Option<Instant>,
    last_bandwidth_emit: Option<(Instant, u64)>,
    last_buffer_emit: Option<(Instant, Option<Duration>)>,
}

impl AbrController {
    /// Minimum delay between `AbrEvent::ThroughputSample` emits (fixed).
    const MIN_THROUGHPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

    /// Create a new controller with the default [`ThroughputEstimator`].
    #[must_use]
    pub fn new(settings: AbrSettings) -> Arc<Self> {
        Self::with_estimator(settings, Arc::new(ThroughputEstimator::new()))
    }

    /// Create a new controller with a custom estimator. Used in tests to
    /// inject a mock.
    #[must_use]
    pub fn with_estimator(settings: AbrSettings, estimator: Arc<dyn Estimator>) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            settings,
            estimator,
            peers: Mutex::new(HashMap::new()),
            next_peer_id: AtomicU64::new(0),
            self_weak: weak.clone(),
        })
    }

    /// Settings snapshot.
    #[must_use]
    pub fn settings(&self) -> &AbrSettings {
        &self.settings
    }

    /// Current shared bandwidth estimate.
    #[must_use]
    pub fn current_bandwidth_estimate_bps(&self) -> Option<u64> {
        self.estimator.estimate_bps()
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
            state: state.clone(),
            bus: Arc::clone(&bus),
            bytes_downloaded: AtomicU64::new(0),
            variants_registered_published: AtomicBool::new(false),
            warmup_completed: AtomicBool::new(false),
            last_variant_switch: Mutex::new(None),
            incoherence_cancel: Mutex::new(None),
            throttle: Mutex::new(EventThrottleCache::default()),
        });
        self.peers.lock_sync().insert(id, entry);
        AbrHandle::new(Arc::clone(self), id, state, bus)
    }

    /// Called from [`AbrHandle::drop`].
    pub(crate) fn unregister(&self, id: AbrPeerId) {
        if let Some(entry) = self.peers.lock_sync().remove(&id)
            && let Some(token) = entry.incoherence_cancel.lock_sync().take()
        {
            token.cancel();
        }
    }

    /// Test helper: is `id` registered?
    #[cfg(any(test, feature = "internal"))]
    #[must_use]
    pub fn has_peer(&self, id: AbrPeerId) -> bool {
        self.peers.lock_sync().contains_key(&id)
    }

    /// Record a bandwidth sample for `peer_id`. Called by the Downloader
    /// when a fetch completes. Also triggers a `tick` for the peer.
    pub fn record_bandwidth(
        &self,
        peer_id: AbrPeerId,
        bytes: u64,
        fetch_duration: Duration,
        source: BandwidthSource,
    ) {
        if fetch_duration.as_millis() < self.settings.min_throughput_record_ms {
            return;
        }
        self.estimator.push_sample(bytes, fetch_duration, source);

        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };

        let prev = entry.bytes_downloaded.fetch_add(bytes, Ordering::AcqRel);
        let now_total = prev.saturating_add(bytes);

        let now = Instant::now();
        let bus = entry.bus();
        if let Some(ref bus) = bus {
            let mut throttle = entry.throttle.lock_sync();
            let emit = throttle
                .last_throughput_sample_at
                .is_none_or(|t| now.duration_since(t) >= Self::MIN_THROUGHPUT_SAMPLE_INTERVAL);
            if emit {
                throttle.last_throughput_sample_at = Some(now);
                drop(throttle);
                let bps = bytes_per_second(bytes, fetch_duration);
                bus.publish(AbrEvent::ThroughputSample {
                    bytes_per_second: bps,
                    source,
                });
            }
        }

        if !entry.warmup_completed.load(Ordering::Acquire)
            && now_total >= self.settings.warmup_min_bytes
            && entry
                .warmup_completed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            && let Some(ref bus) = bus
        {
            bus.publish(AbrEvent::WarmupCompleted);
        }

        self.tick(peer_id, now);
    }

    /// Test helper — drive a tick from outside the downloader.
    #[cfg(any(test, feature = "internal"))]
    pub fn tick_for_testing(&self, peer_id: AbrPeerId, now: Instant) {
        self.tick(peer_id, now);
    }

    fn tick(&self, peer_id: AbrPeerId, now: Instant) {
        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };
        let Some(peer) = entry.peer_weak.upgrade() else {
            return;
        };

        let bus = entry.bus();
        let variants = peer.variants();
        let progress = peer.progress();
        let buffer_ahead = progress.map(|p| {
            p.download_head_playback_time
                .saturating_sub(p.reader_playback_time)
        });
        let bytes_downloaded = entry.bytes_downloaded.load(Ordering::Acquire);
        let estimate_bps = self.estimator.estimate_bps();

        if !entry.variants_registered_published.load(Ordering::Acquire)
            && let Some(ref bus) = bus
        {
            let infos: Vec<VariantInfo> = variants
                .iter()
                .map(|v| VariantInfo {
                    index: v.variant_index,
                    bandwidth_bps: Some(v.bandwidth_bps),
                    name: None,
                    codecs: None,
                    container: None,
                })
                .collect();
            let initial = entry
                .state
                .as_ref()
                .map_or(0, |s| s.current_variant_index());
            bus.publish(AbrEvent::VariantsRegistered {
                variants: infos,
                initial,
            });
            entry
                .variants_registered_published
                .store(true, Ordering::Release);
        }

        self.emit_throttled(&entry, &bus, now, estimate_bps, buffer_ahead);

        let Some(state) = entry.state.as_ref() else {
            return;
        };

        let view = AbrView {
            estimate_bps,
            buffer_ahead,
            bytes_downloaded,
            variants: &variants,
            settings: &self.settings,
        };
        let decision = state.decide(&view, now);

        if decision.changed {
            let current_before = state.current_variant_index();
            state.apply(&decision, now);
            if let Some(ref bus) = bus {
                bus.publish(AbrEvent::VariantApplied {
                    from: current_before,
                    to: decision.target_variant_index,
                    reason: decision.reason,
                });
            }
            let reader_pt = progress.map_or(Duration::ZERO, |p| p.reader_playback_time);
            self.schedule_incoherence_watch(peer_id, reader_pt, now);
        } else if decision.reason != AbrReason::AlreadyOptimal
            && let Some(ref bus) = bus
        {
            bus.publish(AbrEvent::DecisionSkipped {
                reason: decision.reason,
            });
        }
    }

    fn emit_throttled(
        &self,
        entry: &PeerEntry,
        bus: &Option<EventBus>,
        now: Instant,
        estimate_bps: Option<u64>,
        buffer_ahead: Option<Duration>,
    ) {
        let Some(bus) = bus else {
            return;
        };
        let mut throttle = entry.throttle.lock_sync();

        if let Some(bps) = estimate_bps {
            let should_emit = match throttle.last_bandwidth_emit {
                None => true,
                Some((t, prev)) => {
                    let time_ok =
                        now.duration_since(t) >= self.settings.bandwidth_emit_min_interval;
                    let delta_ok =
                        relative_delta(prev, bps) >= self.settings.bandwidth_emit_min_delta_ratio;
                    time_ok || delta_ok
                }
            };
            if should_emit {
                throttle.last_bandwidth_emit = Some((now, bps));
                bus.publish(AbrEvent::BandwidthEstimate { bps });
            }
        }

        let should_emit_buffer = match throttle.last_buffer_emit {
            None => true,
            Some((t, prev)) => {
                let time_ok = now.duration_since(t) >= self.settings.buffer_emit_min_interval;
                let transition = prev.is_some() != buffer_ahead.is_some();
                let delta_ok = match (prev, buffer_ahead) {
                    (Some(a), Some(b)) => {
                        duration_delta(a, b) >= self.settings.buffer_emit_min_delta
                    }
                    _ => true,
                };
                transition || (time_ok && delta_ok)
            }
        };
        if should_emit_buffer {
            throttle.last_buffer_emit = Some((now, buffer_ahead));
            drop(throttle);
            bus.publish(AbrEvent::BufferAhead {
                ahead: buffer_ahead,
            });
        }
    }

    fn schedule_incoherence_watch(&self, peer_id: AbrPeerId, reader_pt: Duration, now: Instant) {
        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };
        let token = CancellationToken::new();
        {
            let mut slot = entry.last_variant_switch.lock_sync();
            *slot = Some((now, reader_pt));
        }
        {
            let mut slot = entry.incoherence_cancel.lock_sync();
            if let Some(prev) = slot.replace(token.clone()) {
                prev.cancel();
            }
        }

        let deadline = self.settings.incoherence_deadline;
        let controller_weak = self.self_weak.clone();
        kithara_platform::tokio::spawn(async move {
            kithara_platform::tokio::select! {
                () = kithara_platform::tokio::time::sleep(deadline) => {}
                () = token.cancelled() => return,
            }
            let Some(ctrl) = controller_weak.upgrade() else {
                return;
            };
            ctrl.check_incoherence(peer_id, reader_pt, now);
        });
    }

    fn check_incoherence(
        &self,
        peer_id: AbrPeerId,
        reader_pt_at_switch: Duration,
        switch_at: Instant,
    ) {
        let Some(entry) = self.peer_entry(peer_id) else {
            return;
        };
        if entry.state.as_ref().is_some_and(|s| s.is_locked()) {
            return;
        }
        let Some(peer) = entry.peer_weak.upgrade() else {
            return;
        };
        let Some(progress) = peer.progress() else {
            return;
        };
        if progress.reader_playback_time <= reader_pt_at_switch
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Incoherence {
                description: format!(
                    "reader stuck at {reader_pt_at_switch:?} after variant switch"
                ),
                elapsed: switch_at.elapsed(),
            });
        }
    }

    pub(crate) fn on_mode_changed(&self, peer_id: AbrPeerId, mode: AbrMode) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::ModeChanged { mode });
        }
        self.tick(peer_id, Instant::now());
    }

    pub(crate) fn on_max_bandwidth_cap_changed(&self, peer_id: AbrPeerId, cap: Option<u64>) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::MaxBandwidthCapChanged { cap });
        }
        self.tick(peer_id, Instant::now());
    }

    pub(crate) fn on_locked(&self, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Locked);
        }
    }

    pub(crate) fn on_unlocked(&self, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Unlocked);
        }
        self.tick(peer_id, Instant::now());
    }

    fn peer_entry(&self, id: AbrPeerId) -> Option<Arc<PeerEntry>> {
        self.peers.lock_sync().get(&id).cloned()
    }

    fn allocate_peer_id(&self) -> AbrPeerId {
        let raw = self
            .next_peer_id
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        AbrPeerId::new(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| NonZeroU64::new(1).expect("1 is always non-zero")),
        )
    }
}

fn bytes_per_second(bytes: u64, duration: Duration) -> f64 {
    let secs = duration.as_secs_f64().max(f64::EPSILON);
    #[expect(clippy::cast_precision_loss)]
    let bytes_f = bytes as f64;
    bytes_f / secs
}

fn relative_delta(prev: u64, now: u64) -> f64 {
    if prev == 0 {
        return f64::INFINITY;
    }
    #[expect(clippy::cast_precision_loss)]
    let diff = (i128::from(prev) - i128::from(now)).unsigned_abs() as f64;
    #[expect(clippy::cast_precision_loss)]
    let base = prev as f64;
    diff / base
}

fn duration_delta(a: Duration, b: Duration) -> Duration {
    a.abs_diff(b)
}
