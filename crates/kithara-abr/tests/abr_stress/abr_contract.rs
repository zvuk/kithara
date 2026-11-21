//! ABR state-machine contract tests.

use kithara_abr::{
    AbrMode, AbrReason, AbrSettings, AbrState, AbrView, VariantDuration, VariantIndex, VariantInfo,
};
use kithara_platform::{
    sync::Arc,
    time::{Duration, Instant},
};
use kithara_test_utils::kithara;

fn fresh_state(idx: usize) -> Arc<AbrState> {
    Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(idx)))))
}

#[kithara::test(tokio, native)]
#[serial_test::serial]
async fn auto_commit_flips_active_variant() {
    let state = fresh_state(0);
    state.request_target(VariantIndex::new(3), AbrReason::AlreadyOptimal);
    assert_eq!(state.pending_target(), Some(VariantIndex::new(3)));

    let claim = state
        .claim_pending_decision(state.current_variant_index())
        .expect("auto peek must surface a decision when pending differs from current");

    assert!(
        claim.decision().changed(),
        "auto commit must mark the decision as a real change"
    );
    assert_eq!(claim.decision().target(), VariantIndex::new(3));
    assert_eq!(
        state.current_variant_index(),
        VariantIndex::new(0),
        "a claim must not flip current_variant - commit_pending does"
    );

    assert!(state.commit_pending(claim, Instant::now()));
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

#[kithara::test(tokio, native)]
#[serial_test::serial]
async fn manual_set_mode_uses_same_commit_path() {
    let state = fresh_state(0);
    state.set_mode(AbrMode::Manual(VariantIndex::new(2)));
    // Manual mode surfaces its target through the same pending-decision
    // channel Auto uses, so claim_pending_decision + commit_pending drive
    // both with one path. The scheduler doesn't need to special-case
    // manual switches.
    state.request_target(VariantIndex::new(2), AbrReason::AlreadyOptimal);

    let claim = state
        .claim_pending_decision(state.current_variant_index())
        .expect("manual commit must travel the same boundary path as auto");
    assert!(state.commit_pending(claim, Instant::now()));

    assert!(claim.decision().changed());
    assert_eq!(claim.decision().target(), VariantIndex::new(2));
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    assert_eq!(state.mode(), AbrMode::Manual(VariantIndex::new(2)));
}

#[kithara::test(tokio, native)]
#[serial_test::serial]
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
    let claim = state
        .claim_pending_decision(state.current_variant_index())
        .expect("unlock releases the fence — same pending intent must surface");
    assert!(state.commit_pending(claim, Instant::now()));
    assert!(claim.decision().changed());
    assert_eq!(claim.decision().target(), VariantIndex::new(2));
}

#[kithara::test(tokio, native)]
#[serial_test::serial]
async fn no_op_switch_never_surfaces_a_decision() {
    // A no-op pending (target == current) must NOT surface - peek returns
    // None so commit_pending never fires. `AbrDecision::changed()` is the
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

#[kithara::test]
fn published_rescue_switch_lifts_the_escape_from_the_new_variant() {
    let variants: Vec<VariantInfo> = [300_000, 900_000, 3_000_000]
        .into_iter()
        .enumerate()
        .map(|(index, bps)| VariantInfo {
            bandwidth_bps: Some(bps),
            codecs: None,
            container: None,
            name: None,
            duration: VariantDuration::Unknown,
            variant_index: VariantIndex::new(index),
        })
        .collect();
    let settings = AbrSettings::builder()
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .throughput_safety_factor(1.0)
        .build();
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(2)))));
    let view = AbrView {
        settings: &settings,
        variants: &variants,
        buffer_ahead: None,
        estimate_bps: Some(1_800_000),
        bytes_downloaded: 4 * 1024 * 1024,
    };

    state.mark_escape();
    let rescue = state.decide(&view, Instant::now());
    assert_eq!(rescue.reason(), AbrReason::EscapeStalled);
    assert_eq!(rescue.target(), VariantIndex::new(1));
    state.request_target(rescue.target(), rescue.reason());

    let stale = state
        .claim_pending_decision(VariantIndex::new(0))
        .expect("a claim formed from another variant still names the live request");
    assert!(!state.commit_pending(stale, Instant::now()));
    assert!(
        state.is_escaping(),
        "a refused stale claim must keep the escape mark"
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    assert_eq!(state.pending_target(), Some(VariantIndex::new(1)));

    let claim = state
        .claim_pending_decision(state.current_variant_index())
        .expect("the rescue request must be claimable from the stalled variant");
    state.lock();
    assert!(!state.commit_pending(claim, Instant::now()));
    assert!(
        state.is_escaping(),
        "a commit refused under the lock must keep the escape mark"
    );
    assert_eq!(state.current_variant_index(), VariantIndex::new(2));
    state.unlock();

    let claim = state
        .claim_pending_decision(state.current_variant_index())
        .expect("the rescue request must outlive the lock");
    assert!(state.commit_pending(claim, Instant::now()));
    assert_eq!(state.current_variant_index(), VariantIndex::new(1));
    assert_eq!(state.pending_target(), None);
    assert!(
        !state.is_escaping(),
        "a published rescue must lift the escape mark"
    );

    let next = state.decide(&view, Instant::now());
    assert!(!next.changed());
    assert_eq!(next.reason(), AbrReason::AlreadyOptimal);
}
