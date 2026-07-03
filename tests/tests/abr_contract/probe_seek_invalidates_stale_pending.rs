use kithara::{
    self,
    abr::{AbrMode, AbrReason, AbrState, VariantIndex},
    platform::time::Duration,
};

fn fresh_state(initial: usize) -> AbrState {
    AbrState::new(AbrMode::Auto(Some(VariantIndex::new(initial))))
}

/// A seek epoch drops a pending switch that was chosen against pre-seek
/// throughput. These reasons go stale across a position jump and must
/// not commit on the first post-seek boundary (the prod HLS-AAC2
/// variant-switch hang signature).
const THROUGHPUT_DRIVEN: [AbrReason; 3] = [
    AbrReason::UpSwitch,
    AbrReason::DownSwitch,
    AbrReason::UrgentDownSwitch,
];

/// User-driven or first-pick intents survive a seek — a position jump
/// does not invalidate what the user asked for or the initial variant.
const SEEK_DURABLE: [AbrReason; 2] = [AbrReason::ManualOverride, AbrReason::Initial];

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn seek_invalidates_throughput_driven_pending() {
    for reason in THROUGHPUT_DRIVEN {
        let state = fresh_state(0);
        state.request_target(VariantIndex::new(3), reason);
        assert_eq!(
            state.pending_target(),
            Some(VariantIndex::new(3)),
            "{reason:?}: request_target must record the pending intent"
        );

        state.invalidate_pending();

        assert_eq!(
            state.pending_target(),
            None,
            "{reason:?}: a throughput-driven pending must be dropped by a seek epoch"
        );
        assert!(
            state
                .peek_pending_decision(state.current_variant_index())
                .is_none(),
            "{reason:?}: a dropped pending must not surface a boundary decision"
        );
    }
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn seek_preserves_user_and_initial_pending() {
    for reason in SEEK_DURABLE {
        let state = fresh_state(0);
        state.request_target(VariantIndex::new(3), reason);

        state.invalidate_pending();

        assert_eq!(
            state.pending_target(),
            Some(VariantIndex::new(3)),
            "{reason:?}: a seek must not invalidate user-driven / first-pick intent"
        );
        let decision = state
            .peek_pending_decision(state.current_variant_index())
            .expect("preserved pending must still surface a boundary decision");
        assert_eq!(decision.target(), VariantIndex::new(3));
        assert_eq!(decision.reason(), reason);
    }
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn lock_then_invalidate_drops_stale_throughput_across_unlock() {
    // Locking gates *publish* but preserves intent; invalidation is
    // destructive and orthogonal to the lock. A throughput-driven
    // pending invalidated while locked must stay gone after unlock — the
    // lock must not resurrect a stale pre-seek up-switch.
    let state = fresh_state(0);
    state.request_target(VariantIndex::new(2), AbrReason::UpSwitch);

    state.lock();
    assert!(
        state
            .peek_pending_decision(state.current_variant_index())
            .is_none(),
        "locked peek must defer the boundary commit"
    );

    state.invalidate_pending();
    assert_eq!(
        state.pending_target(),
        None,
        "invalidation must drop the stale throughput pending even while locked"
    );

    state.unlock();
    assert!(
        state
            .peek_pending_decision(state.current_variant_index())
            .is_none(),
        "unlock must not resurrect an invalidated pre-seek up-switch"
    );
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn invalidate_pending_on_empty_slot_is_a_noop() {
    let state = fresh_state(1);
    assert_eq!(state.pending_target(), None);
    state.invalidate_pending();
    assert_eq!(
        state.pending_target(),
        None,
        "invalidating an empty slot must stay a no-op, not panic or fabricate intent"
    );
}
