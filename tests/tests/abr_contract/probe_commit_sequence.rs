use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;

fn fresh_state(initial: usize) -> AbrState {
    AbrState::new(AbrMode::Auto(Some(VariantIndex::new(initial))))
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn auto_commit_flips_active_variant() {
    let state = fresh_state(0);
    state.request_target(VariantIndex::new(3), AbrReason::AlreadyOptimal);
    assert_eq!(state.pending_target(), Some(VariantIndex::new(3)));

    let decision = state
        .peek_pending_decision(state.current_variant_index())
        .expect("auto peek must surface a decision when pending differs from current");

    assert!(
        decision.changed(),
        "auto commit must mark the decision as a real change"
    );
    assert_eq!(decision.target(), VariantIndex::new(3));
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(0),
        "peek must not flip current_variant — apply_decision does that"
    );

    state.apply_decision(&decision, Instant::now());
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(3),
        "current variant must follow the committed decision"
    );
    assert_eq!(
        state.pending_target(),
        None,
        "matching pending must drain on apply so a stale intent can't fire twice"
    );
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn manual_set_mode_uses_same_commit_path() {
    let state = fresh_state(0);
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    // Manual mode surfaces its target through the same pending-decision
    // channel Auto uses, so peek_pending_decision + apply_decision drive
    // both with one code path. The scheduler doesn't need to special-case
    // manual switches.
    state.request_target(VariantIndex::new(2), AbrReason::AlreadyOptimal);

    let decision = state
        .peek_pending_decision(state.current_variant_index())
        .expect("manual commit must travel the same boundary path as auto");
    state.apply_decision(&decision, Instant::now());

    assert!(decision.changed());
    assert_eq!(decision.target(), VariantIndex::new(2));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    assert_eq!(state.mode(), AbrMode::Manual(VariantIndex::new(2)));
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn peek_pending_decision_returns_none_during_seek() {
    let state = fresh_state(0);
    state.request_target(VariantIndex::new(2), AbrReason::AlreadyOptimal);

    state.lock();
    assert!(
        state
            .peek_pending_decision(state.current_variant_index())
            .is_none(),
        "ABR locked (seek pending / blender fence) must block boundary commits"
    );
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(0),
        "locked peek must not advance current_variant"
    );
    assert_eq!(
        state.pending_target(),
        Some(VariantIndex::new(2)),
        "locked peek must preserve the pending intent for the next boundary"
    );

    state.unlock();
    let resumed = state
        .peek_pending_decision(state.current_variant_index())
        .expect("unlock releases the fence — same pending intent must surface");
    state.apply_decision(&resumed, Instant::now());
    assert!(resumed.changed());
    assert_eq!(resumed.target(), VariantIndex::new(2));
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn no_op_switch_never_surfaces_a_decision() {
    // A no-op pending (target == current) must NOT surface — peek returns
    // None so apply_decision never fires. `AbrDecision::changed()` is the
    // discriminant the scheduler reads downstream; proving the no-op path
    // stays at `None` is the equivalent contract.
    let state = fresh_state(1);
    state.request_target(VariantIndex::new(1), AbrReason::AlreadyOptimal);

    assert_eq!(state.pending_target(), Some(VariantIndex::new(1)));
    assert!(
        state.peek_pending_decision(VariantIndex::new(1)).is_none(),
        "no-op switch (target == current) must not surface a decision — \
         coord::commit_variant_switch would otherwise call notify_commit \
         for a flip that didn't happen"
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
    // Pending slot is left untouched — peek is read-only. The slot will
    // be consumed only when a new `request_target` overwrites it or when
    // peek's invariants line up against a different current_variant.
    assert_eq!(state.pending_target(), Some(VariantIndex::new(1)));
}
