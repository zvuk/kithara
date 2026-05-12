use std::sync::Arc;

use kithara_events::{AbrEvent, AbrMode, EventBus};
use kithara_platform::{
    RwLock,
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;

use crate::{
    controller::{AbrController, AbrPeerId},
    state::{AbrDecision, AbrError, AbrState},
};

/// Clone-able handle returned by [`AbrController::register`].
///
/// Mirrors the shape of `PeerHandle` in `kithara-stream`: the consumer
/// attaches the track-scoped event bus with [`Self::with_bus`] and keeps
/// the handle alive for the lifetime of the peer. The bus lives inside
/// the handle — peers stay free of event-bus plumbing.
#[derive(Clone)]
pub struct AbrHandle {
    inner: Arc<HandleInner>,
}

pub(crate) struct HandleInner {
    pub(crate) peer_id: AbrPeerId,
    pub(crate) bus: Arc<RwLock<Option<EventBus>>>,
    pub(crate) controller: Arc<AbrController>,
    pub(crate) state: Option<Arc<AbrState>>,
}

impl AbrHandle {
    pub(crate) fn new(
        controller: Arc<AbrController>,
        peer_id: AbrPeerId,
        state: Option<Arc<AbrState>>,
        bus: Arc<RwLock<Option<EventBus>>>,
    ) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                peer_id,
                bus,
                controller,
                state,
            }),
        }
    }

    /// Current variant index — `None` for peers without state.
    #[must_use]
    pub fn current_variant_index(&self) -> Option<usize> {
        self.inner.state.as_ref().map(|s| s.current_variant_index())
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.inner.state.as_ref().is_some_and(|s| s.is_locked())
    }

    /// Lock ABR (used during seek).
    pub fn lock(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            let before = state.lock_count();
            state.lock();
            if before == 0 {
                self.inner.controller.on_locked(self.inner.peer_id);
            }
        }
    }

    #[must_use]
    pub fn peer_id(&self) -> AbrPeerId {
        self.inner.peer_id
    }

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        if let Some(state) = self.inner.state.as_ref() {
            state.set_max_bandwidth_bps(cap);
            self.inner
                .controller
                .on_max_bandwidth_cap_changed(self.inner.peer_id, cap);
        }
    }

    /// Change mode.
    ///
    /// # Errors
    /// Returns [`AbrError::VariantOutOfBounds`] when `mode` is
    /// `AbrMode::Manual(idx)` and `idx` is not in the peer's variant list.
    pub fn set_mode(&self, mode: AbrMode) -> Result<(), AbrError> {
        match self.inner.state.as_ref() {
            Some(state) => {
                state.set_mode(mode)?;
                self.inner
                    .controller
                    .on_mode_changed(self.inner.peer_id, mode);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Release one lock level.
    pub fn unlock(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            state.unlock();
            if state.lock_count() == 0 {
                self.inner.controller.on_unlocked(self.inner.peer_id);
            }
        }
    }

    /// Attach the track-scoped event bus. Stored directly on the handle;
    /// the controller reads it through the shared `Arc` when publishing.
    #[must_use]
    pub fn with_bus(self, bus: EventBus) -> Self {
        *self.inner.bus.lock_sync_write() = Some(bus);
        self
    }

    /// Boundary commit of pending ABR decision. Called by HLS scheduler at fetch boundary.
    ///
    /// Returns `Some(decision)` when a pending decision was committed (state.current updated).
    /// Returns `None` when no pending decision, or `is_locked()` blocks the commit.
    #[kithara::probe]
    pub fn commit_pending(&self, now: Instant) -> Option<AbrDecision> {
        self.inner
            .state
            .as_ref()
            .and_then(|s| s.commit_pending(now))
    }

    /// Side-effects after HLS scheduler committed a variant switch:
    /// emits `VariantApplied` via bus + schedules incoherence watchdog.
    #[kithara::probe(current_before)]
    pub fn notify_commit(
        &self,
        decision: AbrDecision,
        current_before: usize,
        reader_pt: Duration,
        now: Instant,
    ) {
        let bus = self.inner.bus.lock_sync_read().clone();
        if let Some(bus) = bus {
            bus.publish(AbrEvent::VariantApplied {
                from: current_before,
                to: decision.target_variant_index,
                reason: decision.reason,
            });
        }
        self.inner
            .controller
            .schedule_incoherence_watch(self.inner.peer_id, reader_pt, now);
    }
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.controller.unregister(self.peer_id);
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{
        AbrEvent, AbrReason, AbrVariant, DEFAULT_EVENT_BUS_CAPACITY, Event, EventBus,
        VariantDuration,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        Abr, AbrController, AbrSettings, ThroughputEstimator,
        state::{AbrDecision, AbrState},
    };

    fn test_variants_3() -> Vec<AbrVariant> {
        vec![
            AbrVariant {
                variant_index: 0,
                bandwidth_bps: 256_000,
                duration: VariantDuration::Unknown,
            },
            AbrVariant {
                variant_index: 1,
                bandwidth_bps: 512_000,
                duration: VariantDuration::Unknown,
            },
            AbrVariant {
                variant_index: 2,
                bandwidth_bps: 1_024_000,
                duration: VariantDuration::Unknown,
            },
        ]
    }

    fn settings_fast() -> AbrSettings {
        AbrSettings {
            warmup_min_bytes: 0,
            min_switch_interval: Duration::ZERO,
            min_buffer_for_up_switch: Duration::ZERO,
            ..AbrSettings::default()
        }
    }

    struct StatefulPeer {
        state: Arc<AbrState>,
    }
    impl Abr for StatefulPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
        fn variants(&self) -> Vec<AbrVariant> {
            self.state.variants_snapshot()
        }
    }

    #[kithara::test(tokio)]
    async fn commit_pending_happy_path() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(test_variants_3(), AbrMode::Auto(Some(0))));
        let peer: Arc<dyn Abr> = Arc::new(StatefulPeer {
            state: Arc::clone(&state),
        });
        let handle = controller.register(&peer);

        state.request_target(2, AbrReason::UpSwitch);

        let decision = handle.commit_pending(Instant::now());
        let decision = decision.expect("commit_pending must return Some when pending is set");
        assert_eq!(decision.target_variant_index, 2);
        assert_eq!(decision.reason, AbrReason::UpSwitch);
        assert!(decision.did_change);
        assert_eq!(state.current_variant_index(), 2);
    }

    #[kithara::test(tokio)]
    async fn commit_pending_returns_none_when_locked() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(test_variants_3(), AbrMode::Auto(Some(0))));
        let peer: Arc<dyn Abr> = Arc::new(StatefulPeer {
            state: Arc::clone(&state),
        });
        let handle = controller.register(&peer);

        handle.lock();
        state.request_target(2, AbrReason::UpSwitch);

        let decision = handle.commit_pending(Instant::now());
        assert!(
            decision.is_none(),
            "commit_pending must return None while locked"
        );
        assert_eq!(state.current_variant_index(), 0);
        assert_eq!(state.pending_target(), Some(2));
    }

    #[kithara::test(tokio)]
    async fn notify_commit_emits_variant_applied() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(test_variants_3(), AbrMode::Auto(Some(0))));
        let peer: Arc<dyn Abr> = Arc::new(StatefulPeer {
            state: Arc::clone(&state),
        });

        let bus = EventBus::new(DEFAULT_EVENT_BUS_CAPACITY);
        let mut rx = bus.subscribe();
        let handle = controller.register(&peer).with_bus(bus);

        let decision = AbrDecision {
            target_variant_index: 2,
            reason: AbrReason::UpSwitch,
            did_change: true,
        };
        handle.notify_commit(decision, 0, Duration::ZERO, Instant::now());

        let mut seen = false;
        while let Ok(event) = rx.try_recv() {
            if let Event::Abr(AbrEvent::VariantApplied { from, to, reason }) = event {
                assert_eq!(from, 0);
                assert_eq!(to, 2);
                assert_eq!(reason, AbrReason::UpSwitch);
                seen = true;
                break;
            }
        }
        assert!(seen, "expected VariantApplied event on the bus");
    }
}
