use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
    task::Waker,
};

use bon::Builder;
use dashmap::DashMap;
use kithara_events::{AbrEvent, AbrMode, EventBus};
use kithara_macros::Patch;
use kithara_platform::{
    CancelGroup, CancelScope, CancelToken,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use kithara_test_utils::kithara;

use super::peer::PeerEntry;
use crate::{
    abr::Abr,
    estimator::{Estimator, ThroughputEstimator},
    handle::AbrHandle,
};

struct Defaults;

impl Defaults {
    const BANDWIDTH_EMIT_MIN_DELTA_RATIO: f64 = 0.10;
    const BANDWIDTH_EMIT_MIN_INTERVAL: Duration = Duration::from_secs(1);
    const BUFFER_EMIT_MIN_DELTA: Duration = Duration::from_millis(500);
    const BUFFER_EMIT_MIN_INTERVAL: Duration = Duration::from_millis(500);
    const DOWN_HYSTERESIS_RATIO: f64 = 0.8;
    const INITIAL_THROUGHPUT_BPS: u64 = 2_000_000;
    const MIN_BUFFER_FOR_UP_SWITCH: Duration = Duration::from_secs(10);
    const MIN_SWITCH_INTERVAL: Duration = Duration::from_secs(30);
    const THROUGHPUT_SAFETY_FACTOR: f64 = 1.5;
    const THROUGHPUT_SAMPLE_MIN_INTERVAL: Duration = Duration::from_millis(200);
    const UP_HYSTERESIS_RATIO: f64 = 1.3;
    const URGENT_DOWNSWITCH_BUFFER: Duration = Duration::from_secs(5);
}

/// Opaque peer identifier assigned by the ABR controller on `register`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AbrPeerId(NonZeroU64);

impl AbrPeerId {
    /// Construct from a non-zero identifier.
    #[must_use]
    pub const fn new(id: NonZeroU64) -> Self {
        Self(id)
    }
}

impl kithara_test_utils::probe::IntoProbeArg for AbrPeerId {
    fn into_probe_arg(self) -> u64 {
        self.0.get()
    }
}

/// ABR controller settings.
#[derive(Clone, Debug, Builder, Patch)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct AbrSettings {
    /// Minimum interval between `AbrEvent::BandwidthEstimate` emits.
    #[builder(default = Defaults::BANDWIDTH_EMIT_MIN_INTERVAL)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub bandwidth_emit_min_interval: Duration,
    /// Minimum absolute delta between `BufferAhead` emits.
    #[builder(default = Defaults::BUFFER_EMIT_MIN_DELTA)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub buffer_emit_min_delta: Duration,
    /// Minimum interval between `AbrEvent::BufferAhead` emits.
    #[builder(default = Defaults::BUFFER_EMIT_MIN_INTERVAL)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub buffer_emit_min_interval: Duration,
    /// Minimum buffer-ahead required before an up-switch is allowed.
    #[builder(default = Defaults::MIN_BUFFER_FOR_UP_SWITCH)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub min_buffer_for_up_switch: Duration,
    /// Minimum interval between variant switches.
    #[builder(default = Defaults::MIN_SWITCH_INTERVAL)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub min_switch_interval: Duration,
    /// Minimum interval between `AbrEvent::ThroughputSample` emits. Every
    /// sample still reaches the estimator; this bounds only how often the
    /// raw per-fetch rate is published to the bus.
    #[builder(default = Defaults::THROUGHPUT_SAMPLE_MIN_INTERVAL)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub throughput_sample_min_interval: Duration,
    /// Buffer-ahead at or below this threshold forces an urgent down-switch.
    #[builder(default = Defaults::URGENT_DOWNSWITCH_BUFFER)]
    #[patch(attribute(serde(with = "humantime_serde::option")))]
    pub urgent_downswitch_buffer: Duration,
    /// Optional parent cancellation token for the controller scope.
    ///
    /// `Some` derives a child scope from the supplied parent; `None` gives the
    /// controller a standalone scope.
    #[patch(skip)]
    pub cancel: Option<CancelToken>,
    /// Seed throughput estimate (bps) applied at controller construction.
    #[builder(required, default = Some(Defaults::INITIAL_THROUGHPUT_BPS))]
    pub initial_throughput_bps: Option<u64>,
    /// Global data-saver cap.
    pub max_bandwidth_bps: Option<u64>,
    /// Minimum relative delta (0.0–1.0) between `BandwidthEstimate` emits.
    #[builder(default = Defaults::BANDWIDTH_EMIT_MIN_DELTA_RATIO)]
    pub bandwidth_emit_min_delta_ratio: f64,
    /// Hysteresis ratio for down-switch.
    #[builder(default = Defaults::DOWN_HYSTERESIS_RATIO)]
    pub down_hysteresis_ratio: f64,
    /// Safety factor applied to the throughput estimate before comparing.
    #[builder(default = Defaults::THROUGHPUT_SAFETY_FACTOR)]
    pub throughput_safety_factor: f64,
    /// Hysteresis ratio for up-switch.
    #[builder(default = Defaults::UP_HYSTERESIS_RATIO)]
    pub up_hysteresis_ratio: f64,
}

impl Default for AbrSettings {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Shared per-player ABR controller.
///
/// Holds the bandwidth estimator (one per controller) and a map of
/// registered peers. Constructed via [`AbrController::new`]; peers are
/// attached with [`AbrController::register`].
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct AbrController {
    #[field(get)]
    pub(super) settings: AbrSettings,
    pub(super) estimator: Arc<dyn Estimator>,
    pub(super) peers: DashMap<AbrPeerId, Arc<PeerEntry>>,
    pub(super) tick_waker: Mutex<Option<Waker>>,
    next_peer_id: AtomicU64,
    scope: CancelScope,
}

impl AbrController {
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
                .unwrap_or_else(|| NonZeroU64::new(1).expect("BUG: 1 is statically non-zero")),
        )
    }

    pub(crate) fn on_locked(&self, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Locked);
        }
    }

    pub(crate) fn on_max_bandwidth_cap_changed(
        self: &Arc<Self>,
        peer_id: AbrPeerId,
        cap: Option<u64>,
    ) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::MaxBandwidthCapChanged { cap });
        }
        self.tick(peer_id);
    }

    #[kithara::probe(peer_id, mode)]
    pub(crate) fn on_mode_changed(self: &Arc<Self>, peer_id: AbrPeerId, mode: AbrMode) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::ModeChanged { mode });
        }
        self.tick(peer_id);
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(peer) = entry.peer_weak.upgrade()
        {
            peer.wake();
        }
    }

    pub(crate) fn on_unlocked(self: &Arc<Self>, peer_id: AbrPeerId) {
        if let Some(entry) = self.peer_entry(peer_id)
            && let Some(bus) = entry.bus()
        {
            bus.publish(AbrEvent::Unlocked);
        }
        self.tick(peer_id);
    }

    pub(crate) fn peer_entry(&self, id: AbrPeerId) -> Option<Arc<PeerEntry>> {
        self.peers
            .get(&id)
            .map(|entry| Arc::clone(entry.value()))
            .filter(|entry| !entry.cancel.is_cancelled())
    }

    /// Register a peer below the controller scope.
    ///
    /// The controller observes both its own per-registration child and the
    /// protocol token returned by [`Abr::cancel`]. Dropping the returned handle
    /// cancels only the registration child.
    pub fn register(self: &Arc<Self>, peer: &Arc<dyn Abr>) -> AbrHandle {
        let id = self.allocate_peer_id();
        let state = peer.state();
        let peer_weak = Arc::downgrade(peer);
        let registration_cancel = self.scope.token().child();
        let cancel = CancelGroup::new(vec![registration_cancel.clone(), peer.cancel()]);
        let bus: Arc<RwLock<Option<EventBus>>> = Arc::new(RwLock::default());
        let entry = Arc::new(
            PeerEntry::new(peer_weak, Arc::clone(&bus), cancel, registration_cancel)
                .with_state(state.clone()),
        );
        self.peers.insert(id, entry);
        AbrHandle::new(Arc::clone(self), id, state, bus)
    }

    fn seed_estimator(settings: &AbrSettings, estimator: &Arc<dyn Estimator>) {
        if let Some(bps) = settings.initial_throughput_bps {
            estimator.seed_initial_bps(bps);
        }
    }

    /// Called from [`AbrHandle::drop`].
    pub(crate) fn unregister(&self, id: AbrPeerId) {
        if let Some((_, entry)) = self.peers.remove(&id) {
            entry.registration_cancel.cancel();
        }
    }

    /// Create a new controller with a custom estimator. Used in tests to inject
    /// a mock.
    #[must_use]
    pub fn with_estimator(settings: AbrSettings, estimator: Arc<dyn Estimator>) -> Arc<Self> {
        Self::seed_estimator(&settings, &estimator);
        let scope = CancelScope::new(settings.cancel.clone());
        Arc::new(Self {
            settings,
            scope,
            estimator,
            tick_waker: Mutex::default(),
            next_peer_id: AtomicU64::new(0),
            peers: DashMap::new(),
        })
    }
}

impl Drop for AbrController {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

#[cfg(test)]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;

    use super::{AbrSettings, AbrSettingsPatch};

    #[kithara::test(native, flash(false))]
    fn a_patch_writes_only_the_hysteresis_it_names() {
        let patch: AbrSettingsPatch =
            serde_yaml_ng::from_str("up_hysteresis_ratio: 1.8\n").expect("the document types");
        // Seeded away from the built default of 0.8, so the assertion below can
        // tell "left alone" from "reset to the default".
        let mut settings = AbrSettings::builder().down_hysteresis_ratio(0.55).build();

        settings.apply(patch);

        assert!((settings.up_hysteresis_ratio - 1.8).abs() < f64::EPSILON);
        assert!(
            (settings.down_hysteresis_ratio - 0.55).abs() < f64::EPSILON,
            "a silent field must keep its value"
        );
    }

    #[kithara::test(native, flash(false))]
    fn an_already_optional_knob_takes_a_bare_number_from_the_document() {
        let patch: AbrSettingsPatch =
            serde_yaml_ng::from_str("max_bandwidth_bps: 5000000\n").expect("the document types");
        // Seeded away from the built default of `Some(2_000_000)`, so the
        // assertion below can tell "left alone" from "reset to the default".
        let mut settings = AbrSettings::builder()
            .initial_throughput_bps(Some(750_000))
            .build();

        settings.apply(patch);

        assert_eq!(
            settings.max_bandwidth_bps,
            Some(5_000_000),
            "an `Option<u64>` field carries `skip_wrap`, so the document names the number bare"
        );
        assert_eq!(
            settings.initial_throughput_bps,
            Some(750_000),
            "a silent field must keep its value"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_patch_reads_a_duration_knob_from_humantime_text() {
        let patch: AbrSettingsPatch =
            serde_yaml_ng::from_str("min_switch_interval: 45s\n").expect("the document types");
        let mut settings = AbrSettings::builder().build();

        settings.apply(patch);

        assert_eq!(settings.min_switch_interval, Duration::from_secs(45));
    }
}
