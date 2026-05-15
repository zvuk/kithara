//! `commit_pending` mechanics — boundary commit invariants from
//! `.docs/plans/2026-05-12-abr-pull-driven-06-D-track-poll-next.md` and
//! the AbrState contract docs.
//!
//! These tests sit at the AbrState / AbrDecision API level rather than
//! the full HLS+probe pipeline: the four invariants below are
//! data-flow contracts that don't need a Recorder to verify, and the
//! cross-variant byte continuity already has end-to-end coverage in
//! `drm_stream_integrity`.

use kithara_abr::{AbrMode, AbrReason, AbrState};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;

fn fresh_state(initial: usize) -> AbrState {
    AbrState::new(AbrMode::Auto(Some(initial)))
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn auto_commit_flips_active_variant() {
    let state = fresh_state(0);
    state.request_target(3, AbrReason::AlreadyOptimal);
    assert_eq!(state.pending_target(), Some(3));

    let decision = state
        .commit_pending(Instant::now())
        .expect("auto commit must surface a decision when a pending switch differs from current");

    assert!(
        decision.did_change,
        "auto commit must mark the decision as a real change"
    );
    assert_eq!(decision.target_variant_index, 3);
    assert_eq!(
        state.current_variant_index(),
        3,
        "current variant must follow the committed decision"
    );
    assert_eq!(
        state.pending_target(),
        None,
        "pending must drain on commit so a stale intent can't fire twice"
    );
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn manual_set_mode_uses_same_commit_path() {
    let state = fresh_state(0);
    state.set_mode(AbrMode::Manual(2));
    // Manual mode surfaces its target through the same pending-decision
    // channel Auto uses, so commit_pending consumes both with one code
    // path. The scheduler doesn't need to special-case manual switches.
    state.request_target(2, AbrReason::AlreadyOptimal);

    let decision = state
        .commit_pending(Instant::now())
        .expect("manual commit must travel the same boundary path as auto");

    assert!(decision.did_change);
    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(state.current_variant_index(), 2);
    assert!(matches!(state.mode(), AbrMode::Manual(2)));
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn commit_pending_returns_none_during_seek() {
    let state = fresh_state(0);
    state.request_target(2, AbrReason::AlreadyOptimal);

    state.lock();
    let blocked = state.commit_pending(Instant::now());
    assert!(
        blocked.is_none(),
        "ABR locked (seek pending / blender fence) must block boundary commits"
    );
    assert_eq!(
        state.current_variant_index(),
        0,
        "locked commit must not advance current_variant"
    );
    assert_eq!(
        state.pending_target(),
        Some(2),
        "locked commit must preserve the pending intent for the next boundary"
    );

    state.unlock();
    let resumed = state
        .commit_pending(Instant::now())
        .expect("unlock releases the fence — same pending intent must commit");
    assert!(resumed.did_change);
    assert_eq!(resumed.target_variant_index, 2);
}

#[kithara::test(tokio, native, serial, timeout(Duration::from_secs(10)))]
async fn did_change_false_skips_notify() {
    // A no-op pending (target == current) must NOT commit — `commit_pending`
    // returns `None` so `notify_commit` never fires. The `did_change`
    // field on `AbrDecision` is the guard the scheduler reads downstream;
    // proving the no-op path stays at `None` is the equivalent contract.
    let state = fresh_state(1);
    state.request_target(1, AbrReason::AlreadyOptimal);

    assert_eq!(state.pending_target(), Some(1));
    let outcome = state.commit_pending(Instant::now());
    assert!(
        outcome.is_none(),
        "no-op switch (target == current) must not surface a decision — \
         coord::commit_variant_switch would otherwise call notify_commit \
         for a flip that didn't happen"
    );
    assert_eq!(state.current_variant_index(), 1);
    assert_eq!(
        state.pending_target(),
        None,
        "consumed pending must drain even when the decision was a no-op"
    );
}
