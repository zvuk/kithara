use kithara_events::EventBus;
use kithara_platform::sync::{Arc, RwLock};
use kithara_test_utils::kithara;

use crate::{
    AbrEvent, AbrMode, VariantIndex, VariantInfo,
    controller::{AbrController, AbrPeerId},
    state::{AbrDecision, AbrError, AbrState, PendingAbrClaim, PendingAbrDecision},
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

    /// Claim the exact pending request without consuming it.
    #[must_use]
    pub fn claim_pending_decision(&self) -> Option<PendingAbrDecision> {
        let state = self.inner.state.as_ref()?;
        state.claim_pending_decision(state.current_variant_index())
    }

    /// Clear the escape condition — see [`AbrState::clear_escape`]. No-op for
    /// stateless handles.
    pub fn clear_escape(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            state.clear_escape();
        }
    }

    /// Current variant's full metadata (bandwidth, name, codecs,
    /// container, duration shape). Pulled live each call — no caching.
    #[must_use]
    pub fn current_variant(&self) -> Option<VariantInfo> {
        let idx = self.inner.state.as_ref()?.current_variant_index();
        self.variants().into_iter().find(|v| v.variant_index == idx)
    }

    /// Current variant index — `None` for peers without state. Unwrapped to
    /// `usize` at this public boundary: consumers in `kithara-audio` /
    /// `kithara-stream` carry their own (unrelated) index space, so the
    /// typed [`VariantIndex`] stops here.
    #[must_use]
    pub fn current_variant_index(&self) -> Option<usize> {
        self.inner
            .state
            .as_ref()
            .map(|s| s.current_variant_index().get())
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

    /// Flag the active variant as non-delivering — see
    /// [`AbrState::mark_escape`]. The caller must follow with
    /// [`Self::reevaluate`] so a tick observes the flag. No-op when stateless.
    pub fn mark_escape(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            state.mark_escape();
        }
    }

    #[must_use]
    pub fn peer_id(&self) -> AbrPeerId {
        self.inner.peer_id
    }

    /// Observe whether the exact pending intent is absent, locked, or ready.
    #[must_use]
    pub fn pending_claim(&self) -> PendingAbrClaim {
        let Some(state) = self.inner.state.as_ref() else {
            return PendingAbrClaim::Absent;
        };
        state.pending_claim(state.current_variant_index())
    }

    fn publish_variant_applied(&self, decision: AbrDecision, current_before: usize) {
        let bus = self.inner.bus.read().clone();
        if let Some(bus) = bus {
            bus.publish(AbrEvent::VariantApplied {
                from: VariantIndex::new(current_before),
                to: decision.target(),
                reason: decision.reason(),
            });
        }
    }

    /// Trigger an out-of-band ABR re-evaluation. Used by the HLS layer after
    /// [`Self::mark_escape`]: the flag is set under the HLS state lock, but the
    /// tick reads `peer.progress()` (which re-locks that state), so the tick
    /// must fire OUTSIDE the lock. Mirrors the controller's `on_*` hooks.
    #[kithara::probe]
    pub fn reevaluate(&self) {
        self.inner.controller.tick(self.inner.peer_id);
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
        let Some(state) = self.inner.state.as_ref() else {
            return Ok(());
        };
        if let AbrMode::Manual(idx) = mode {
            let entry = self.inner.controller.peer_entry(self.inner.peer_id);
            let peer: Option<Arc<dyn crate::Abr>> = entry.and_then(|e| e.peer_weak.upgrade());
            if let Some(peer) = peer {
                let variants = peer.variants();
                if !variants.iter().any(|v| v.variant_index == idx) {
                    return Err(AbrError::VariantOutOfBounds {
                        requested: idx.get(),
                        available: variants.len(),
                    });
                }
            }
        }
        state.set_mode(mode);
        self.inner
            .controller
            .on_mode_changed(self.inner.peer_id, mode);
        Ok(())
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

    /// Pull the live variant list from the peer. Returns an empty vec
    /// when the peer has been dropped or has no variants — callers
    /// should treat empty the same as "not yet registered".
    #[must_use]
    pub fn variants(&self) -> Vec<VariantInfo> {
        self.inner
            .controller
            .peer_entry(self.inner.peer_id)
            .and_then(|e| e.peer_weak.upgrade())
            .map(|peer| peer.variants())
            .unwrap_or_default()
    }

    /// Attach the track-scoped event bus. Stored directly on the handle;
    /// the controller reads it through the shared `Arc` when publishing.
    #[must_use]
    pub fn with_bus(self, bus: EventBus) -> Self {
        *self.inner.bus.write() = Some(bus);
        self
    }

    delegate::delegate! {
        to self.inner.state {
            /// `true` while the active variant is flagged non-delivering — see
            /// [`AbrState::is_escaping`].
            #[must_use]
            #[expr($.is_some_and(|s| s.is_escaping()))]
            #[call(as_ref)]
            pub fn is_escaping(&self) -> bool;
            #[must_use]
            #[expr($.is_some_and(|s| s.is_locked()))]
            #[call(as_ref)]
            pub fn is_locked(&self) -> bool;
            /// Current ABR mode (Auto / Manual). `None` for peers without state.
            #[must_use]
            #[expr($.map(|s| s.mode()))]
            #[call(as_ref)]
            pub fn mode(&self) -> Option<AbrMode>;
        }
        to self {
            /// Side-effects after an exact transition promoted its incoming
            /// generation: emits `VariantApplied` via bus and nothing else.
            ///
            /// The caller is the audio worker, which is not a runtime thread, so this
            /// path must not schedule async work. The stuck-reader watchdog is a
            /// boundary-switch diagnostic and does not apply here: exact promotion
            /// already proved the incoming reader produced staged PCM.
            #[kithara::probe(current_before)]
            #[call(publish_variant_applied)]
            pub fn notify_exact_commit(&self, decision: AbrDecision, current_before: usize);
            /// Read-only: peek at the pending boundary commit. Mirrors
            /// [`AbrState::peek_pending_decision`].
            #[must_use]
            #[kithara::probe]
            #[expr($.map(PendingAbrDecision::decision))]
            #[call(claim_pending_decision)]
            pub fn peek_pending_decision(&self) -> Option<AbrDecision>;
        }
    }
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.controller.unregister(self.peer_id);
    }
}

#[cfg(test)]
mod tests {
    use kithara_events::{DEFAULT_EVENT_BUS_CAPACITY, Envelope, EventBus};
    use kithara_platform::{
        CancelToken,
        time::{Duration, Instant},
    };
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        Abr, AbrController, AbrEvent, AbrMock, AbrReason, AbrSettings, ThroughputEstimator,
        VariantDuration, VariantIndex, VariantInfo,
        state::{AbrDecision, AbrState},
    };

    fn test_variants_3() -> Vec<VariantInfo> {
        vec![
            VariantInfo {
                variant_index: VariantIndex::new(0),
                bandwidth_bps: Some(256_000),
                duration: VariantDuration::Unknown,
                name: None,
                codecs: None,
                container: None,
            },
            VariantInfo {
                variant_index: VariantIndex::new(1),
                bandwidth_bps: Some(512_000),
                duration: VariantDuration::Unknown,
                name: None,
                codecs: None,
                container: None,
            },
            VariantInfo {
                variant_index: VariantIndex::new(2),
                bandwidth_bps: Some(1_024_000),
                duration: VariantDuration::Unknown,
                name: None,
                codecs: None,
                container: None,
            },
        ]
    }

    fn settings_fast() -> AbrSettings {
        AbrSettings::builder()
            .min_switch_interval(Duration::ZERO)
            .min_buffer_for_up_switch(Duration::ZERO)
            .build()
    }

    /// Peer that answers with the state it is given. `variants` keeps its
    /// `Abr` default — unimock rejects a clause no test reaches, so the
    /// variant-reading tests use [`abr_peer_with_variants`] instead.
    fn abr_peer(state: &Arc<AbrState>) -> Arc<dyn Abr> {
        Arc::new(Unimock::new((
            AbrMock::cancel
                .each_call(matching!())
                .returns(CancelToken::never()),
            AbrMock::state
                .each_call(matching!())
                .returns(Some(Arc::clone(state))),
        )))
    }

    /// [`abr_peer`] plus the canonical 3-variant fixture.
    fn abr_peer_with_variants(state: &Arc<AbrState>) -> Arc<dyn Abr> {
        Arc::new(Unimock::new((
            AbrMock::cancel
                .each_call(matching!())
                .returns(CancelToken::never()),
            AbrMock::state
                .each_call(matching!())
                .returns(Some(Arc::clone(state))),
            AbrMock::variants
                .each_call(matching!())
                .returns(test_variants_3()),
        )))
    }

    #[kithara::test(tokio)]
    async fn handle_observes_legacy_publication_from_the_state_owner() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let publisher = state.publisher();
        let peer = abr_peer(&state);
        let handle = controller.register(&peer);

        state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);

        let decision = handle
            .peek_pending_decision()
            .expect("peek must return Some when pending is set");
        assert_eq!(decision.target(), VariantIndex::new(2));
        assert_eq!(decision.reason(), AbrReason::UpSwitch);
        assert!(decision.changed());
        assert_eq!(
            state.current_variant_index(),
            VariantIndex::new(0),
            "peek must not mutate"
        );

        assert!(
            publisher.commit_pending(
                state
                    .claim_pending_decision(state.current_variant_index())
                    .expect("pending claim"),
                Instant::now(),
            )
        );
        assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    }

    #[kithara::test(tokio)]
    async fn handle_observes_exact_publication_from_the_state_owner() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let publisher = state.publisher();
        let peer = abr_peer(&state);
        let handle = controller.register(&peer);

        state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);
        let claim = handle
            .claim_pending_decision()
            .expect("pending request must be claimable");

        assert!(publisher.commit_pending(claim, Instant::now()));
        assert_eq!(state.current_variant_index(), VariantIndex::new(2));
        assert!(handle.claim_pending_decision().is_none());
    }

    #[kithara::test(tokio)]
    async fn peek_pending_decision_returns_none_when_locked() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let peer = abr_peer(&state);
        let handle = controller.register(&peer);

        handle.lock();
        state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);

        assert!(
            handle.peek_pending_decision().is_none(),
            "peek must return None while locked"
        );
        assert_eq!(state.current_variant_index(), VariantIndex::new(0));
        assert_eq!(state.pending_target(), Some(VariantIndex::new(2)));
    }

    #[kithara::test(tokio)]
    async fn handle_pulls_live_variants_from_peer() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(1)))));
        let peer = abr_peer_with_variants(&state);
        let handle = controller.register(&peer);

        let variants = handle.variants();
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[2].bandwidth_bps, Some(1_024_000));

        let current = handle.current_variant().expect("current variant");
        assert_eq!(current.variant_index, VariantIndex::new(1));
        assert_eq!(current.bandwidth_bps, Some(512_000));

        state.apply_decision(
            &AbrDecision::UpSwitch {
                from: VariantIndex::new(1),
                to: VariantIndex::new(2),
                reason: AbrReason::UpSwitch,
            },
            Instant::now(),
        );
        let after = handle
            .current_variant()
            .expect("current variant after switch");
        assert_eq!(after.variant_index, VariantIndex::new(2));
        assert_eq!(after.bandwidth_bps, Some(1_024_000));
    }

    #[kithara::test(tokio)]
    async fn handle_returns_empty_variants_when_peer_dropped() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let handle = {
            let peer = abr_peer_with_variants(&state);
            let h = controller.register(&peer);
            assert_eq!(h.variants().len(), 3);
            h
        };
        assert!(
            handle.variants().is_empty(),
            "Weak<Abr>::upgrade fails after peer drop — variants() must collapse to empty"
        );
        assert!(handle.current_variant().is_none());
    }
    /// Exact promotion runs on the audio worker, which is not a runtime
    /// thread. Deliberately *not* a `tokio` test: a reactor here would hide a
    /// `task::spawn` creeping back into this path.
    #[kithara::test]
    fn notify_exact_commit_emits_variant_applied_without_a_runtime() {
        let controller = AbrController::with_estimator(
            settings_fast(),
            Arc::new(ThroughputEstimator::new()) as Arc<_>,
        );
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let peer = abr_peer(&state);

        let bus = EventBus::new(DEFAULT_EVENT_BUS_CAPACITY);
        let mut rx = bus.subscribe();
        let handle = controller.register(&peer).with_bus(bus);

        let decision = AbrDecision::UpSwitch {
            from: VariantIndex::new(0),
            to: VariantIndex::new(2),
            reason: AbrReason::UpSwitch,
        };
        handle.notify_exact_commit(decision, 0);

        let found =
            std::iter::from_fn(|| rx.try_recv().ok()).find_map(|Envelope { event, .. }| {
                if let AbrEvent::VariantApplied { from, to, reason } = event {
                    assert_eq!(from, VariantIndex::new(0));
                    assert_eq!(to, VariantIndex::new(2));
                    assert_eq!(reason, AbrReason::UpSwitch);
                    Some(())
                } else {
                    None
                }
            });
        assert!(found.is_some(), "expected VariantApplied event on the bus");
    }
}
