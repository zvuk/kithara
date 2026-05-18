use std::{sync::Arc, time::Duration as StdDuration};

use kithara_events::{AbrMode, AbrReason, BandwidthSource, VariantDuration, VariantInfo};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;
use proptest::prelude::*;

use super::{AbrDecision, AbrState, AbrView};
use crate::{Abr, AbrController, AbrSettings, ThroughputEstimator};

/// Canonical 3-variant fixture used by every test in this module. Private
/// to the test module so it never leaks into the public API.
fn test_variants_3() -> Vec<VariantInfo> {
    vec![
        VariantInfo {
            variant_index: 0,
            bandwidth_bps: Some(256_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
        VariantInfo {
            variant_index: 1,
            bandwidth_bps: Some(512_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
        VariantInfo {
            variant_index: 2,
            bandwidth_bps: Some(1_024_000),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        },
    ]
}

fn settings_fast() -> AbrSettings {
    AbrSettings {
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    }
}

fn view_with_bw<'a>(
    bps: Option<u64>,
    variants: &'a [VariantInfo],
    settings: &'a AbrSettings,
) -> AbrView<'a> {
    AbrView {
        variants,
        settings,
        estimate_bps: bps,
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
    }
}

#[kithara::test]
fn decide_locked_never_switches() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.lock();
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(10_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert!(!d.did_change);
    assert_eq!(d.reason, AbrReason::Locked);
}

#[kithara::test]
fn decide_many_samples_during_lock_never_switches() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    let initial = state.current_variant_index();
    state.lock();
    let variants = test_variants_3();
    let settings = settings_fast();
    for i in 0..100u64 {
        let view = view_with_bw(Some(10_000_000 * (i + 1)), &variants, &settings);
        let _ = state.decide(&view, Instant::now() + Duration::from_secs(i * 60));
    }
    assert_eq!(state.current_variant_index(), initial);
}

#[kithara::test]
fn decide_manual_mode_always_target() {
    let state = AbrState::new(AbrMode::Manual(2));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::ManualOverride);
    assert_eq!(d.target_variant_index, 2);
}

#[kithara::test]
fn decide_no_estimate_stays_put() {
    let state = AbrState::new(AbrMode::Auto(Some(1)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::NoEstimate);
    assert!(!d.did_change);
}

#[kithara::test]
fn decide_upswitch_when_bandwidth_allows() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(3_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::UpSwitch);
    assert_eq!(d.target_variant_index, 2);
}

#[kithara::test]
fn decide_downswitch_when_bandwidth_drops() {
    let state = AbrState::new(AbrMode::Auto(Some(2)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(300_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::DownSwitch);
    assert_eq!(d.target_variant_index, 0);
}

#[kithara::test]
fn decide_urgent_downswitch_when_buffer_low() {
    let state = AbrState::new(AbrMode::Auto(Some(2)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        urgent_downswitch_buffer: Duration::from_secs(5),
        down_hysteresis_ratio: 0.01,
        min_switch_interval: Duration::ZERO,
        ..AbrSettings::default()
    };
    let view = AbrView {
        estimate_bps: Some(700_000),
        buffer_ahead: Some(Duration::from_secs(2)),
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::UrgentDownSwitch);
}

#[kithara::test]
fn decide_buffer_too_low_for_upswitch() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        min_buffer_for_up_switch: Duration::from_secs(10),
        min_switch_interval: Duration::ZERO,
        ..AbrSettings::default()
    };
    let view = AbrView {
        estimate_bps: Some(3_000_000),
        buffer_ahead: Some(Duration::from_secs(2)),
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::BufferTooLowForUpSwitch);
    assert!(!d.did_change);
}

#[kithara::test]
fn apply_updates_current_variant_and_timestamp() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.apply(
        &AbrDecision {
            reason: AbrReason::UpSwitch,
            did_change: true,
            target_variant_index: 2,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), 2);
}

#[kithara::test]
fn apply_noop_when_same_variant() {
    let state = AbrState::new(AbrMode::Auto(Some(1)));
    state.apply(
        &AbrDecision {
            reason: AbrReason::AlreadyOptimal,
            did_change: false,
            target_variant_index: 1,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), 1);
}

// `set_mode_rejects_out_of_bounds_manual` removed — variant validation
// is now `AbrHandle::set_mode`'s responsibility (variants live on the
// peer, not the state). See handle.rs tests for the corresponding cover.

#[kithara::test]
fn lock_is_refcounted() {
    let state = AbrState::new(AbrMode::Auto(None));
    state.lock();
    state.lock();
    assert!(state.is_locked());
    state.unlock();
    assert!(state.is_locked());
    state.unlock();
    assert!(!state.is_locked());
}

// Phase 2 of the two-cursor refactor: boundary-commit primitive
// (`request_target` / `commit_pending` / `pending_target`).

#[kithara::test]
fn pending_target_is_empty_on_fresh_state() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    assert_eq!(state.pending_target(), None);
}

#[kithara::test]
fn request_target_records_intent_without_committing() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(2, AbrReason::UpSwitch);
    assert_eq!(state.pending_target(), Some(2));
    assert_eq!(
        state.current_variant_index(),
        0,
        "request_target must not move current_variant; commit_pending owns that step"
    );
}

#[kithara::test]
fn request_target_replace_pending_latest_wins() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(1, AbrReason::UpSwitch);
    state.request_target(2, AbrReason::UpSwitch);
    assert_eq!(
        state.pending_target(),
        Some(2),
        "second request_target must replace the first (latest-wins semantics)"
    );
}

#[kithara::test]
fn peek_pending_decision_returns_none_when_no_request() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    assert!(state.peek_pending_decision(0).is_none());
    assert_eq!(state.current_variant_index(), 0);
}

#[kithara::test]
fn peek_pending_decision_does_not_mutate_current_variant() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(2, AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(0)
        .expect("pending request must produce a decision");
    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(decision.reason, AbrReason::UpSwitch);
    assert!(decision.did_change);
    assert_eq!(state.current_variant_index(), 0, "peek must not mutate");
    assert_eq!(
        state.pending_target(),
        Some(2),
        "peek must not consume pending"
    );
}

#[kithara::test]
fn apply_decision_publishes_and_clears_matching_pending() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(2, AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(0)
        .expect("pending request must produce a decision");
    state.apply_decision(&decision, Instant::now());
    assert_eq!(state.current_variant_index(), 2);
    assert_eq!(
        state.pending_target(),
        None,
        "matching pending must be cleared atomically"
    );
}

#[kithara::test]
fn apply_decision_preserves_pending_overwritten_after_peek() {
    // Race: external request_target overwrites pending between peek and
    // apply. Our captured decision still publishes; the new pending
    // stays for the next boundary commit.
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(2, AbrReason::UpSwitch);
    let decision = state
        .peek_pending_decision(0)
        .expect("pending request must produce a decision");
    state.request_target(3, AbrReason::DownSwitch);
    state.apply_decision(&decision, Instant::now());
    assert_eq!(
        state.current_variant_index(),
        2,
        "captured decision applies regardless of later pending overwrite"
    );
    assert_eq!(
        state.pending_target(),
        Some(3),
        "new pending must survive an apply for a different target"
    );
}

#[kithara::test]
fn peek_pending_decision_honors_is_locked() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.request_target(2, AbrReason::UpSwitch);
    state.lock();
    assert!(
        state.peek_pending_decision(0).is_none(),
        "locked state must not surface pending decisions"
    );
    assert_eq!(state.current_variant_index(), 0);
    assert_eq!(
        state.pending_target(),
        Some(2),
        "deferred decision must remain pending until unlock"
    );
}

#[kithara::test]
fn peek_pending_decision_returns_none_when_target_equals_current() {
    let state = AbrState::new(AbrMode::Auto(Some(1)));
    state.request_target(1, AbrReason::AlreadyOptimal);
    assert!(
        state.peek_pending_decision(1).is_none(),
        "self-switch (target == current) must not produce a decision"
    );
    assert_eq!(state.current_variant_index(), 1);
}

#[kithara::test]
fn apply_decision_after_unlock_applies_still_pending_intent() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    state.lock();
    state.request_target(2, AbrReason::UpSwitch);
    assert!(state.peek_pending_decision(0).is_none());
    state.unlock();
    let decision = state
        .peek_pending_decision(0)
        .expect("post-unlock peek must surface the still-pending intent");
    state.apply_decision(&decision, Instant::now());
    assert_eq!(decision.target_variant_index, 2);
    assert_eq!(state.current_variant_index(), 2);
}

#[kithara::test]
fn min_switch_interval_prevents_oscillation() {
    let state = AbrState::new(AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        min_switch_interval: Duration::from_secs(30),
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    };
    let now = Instant::now();
    let view = AbrView {
        estimate_bps: Some(3_000_000),
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
        variants: &variants,
        settings: &settings,
    };
    let d1 = state.decide(&view, now);
    assert!(d1.did_change);
    state.apply(&d1, now);
    let d2 = state.decide(&view, now + Duration::from_secs(1));
    assert_eq!(d2.reason, AbrReason::MinInterval);
}

/// A locked `AbrState` must never change variant, regardless of bandwidth
/// samples. Parametrized to cover both directions:
/// * locked-at-0 under very-high bandwidth → up-switch rejected
/// * locked-at-2 under very-low bandwidth → down-switch rejected
#[kithara::test]
#[case::rejects_up_switch(0, 20_000_000, 100_000)]
#[case::rejects_down_switch(2, 10_000, 1)]
fn locked_state_rejects_switch(
    #[case] locked_variant: usize,
    #[case] base_bps: u64,
    #[case] step_bps: u64,
) {
    let variants = test_variants_3();
    let settings = settings_fast();
    let state = AbrState::new(AbrMode::Auto(Some(locked_variant)));
    state.lock();

    let now = Instant::now();
    for i in 0..50u64 {
        let v = view_with_bw(Some(base_bps + i * step_bps), &variants, &settings);
        let d = state.decide(&v, now + StdDuration::from_millis(i));
        assert!(!d.did_change, "locked state decided to switch at iter {i}");
    }
    assert_eq!(state.current_variant_index(), locked_variant);
}

struct SeedPeer {
    state: Arc<AbrState>,
    variants: Vec<VariantInfo>,
}

impl Abr for SeedPeer {
    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }
    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.clone()
    }
}

fn audio_variants_4tier() -> Vec<VariantInfo> {
    [66_000_u64, 134_000, 270_000, 900_000]
        .into_iter()
        .enumerate()
        .map(|(i, bps)| VariantInfo {
            variant_index: i,
            bandwidth_bps: Some(bps),
            duration: VariantDuration::Unknown,
            name: None,
            codecs: None,
            container: None,
        })
        .collect()
}

/// Cold-start with the default `initial_throughput_bps = Some(2 Mbps)`
/// seed: first `tick` must request the highest variant fitting under
/// `2 Mbps / safety_factor (1.5) ≈ 1.33 Mbps` — variant 3 (900 kbps).
/// Without the seed (pre-refactor), `estimate_bps()` returns `None` →
/// `AbrReason::NoEstimate` → no pending switch and the player would
/// stay on the initial LQ variant until samples accumulate.
#[kithara::test(tokio)]
async fn auto_mode_with_default_seed_picks_high_variant_on_cold_start() {
    let settings = AbrSettings::builder()
        .initial_throughput_bps(2_000_000)
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build();
    let controller = AbrController::new(settings);
    let state = Arc::new(AbrState::new(AbrMode::Auto(None)));
    let peer: Arc<dyn Abr> = Arc::new(SeedPeer {
        state: Arc::clone(&state),
        variants: audio_variants_4tier(),
    });
    let handle = controller.register(&peer);
    controller.tick(handle.peer_id(), Instant::now());
    assert_eq!(
        state.pending_target(),
        Some(3),
        "cold-start with default 2 Mbps seed must request the top variant"
    );
    drop(handle);
}

/// Explicit opt-out: `initial_throughput_bps = None` preserves the
/// historical cold-start path. First `tick` sees no estimate, returns
/// `AbrReason::NoEstimate`, no pending switch — player stays on the
/// initial variant (0).
#[kithara::test(tokio)]
async fn auto_mode_without_seed_stays_on_initial_variant_on_cold_start() {
    let settings = AbrSettings {
        initial_throughput_bps: None,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    };
    let controller = AbrController::new(settings);
    let state = Arc::new(AbrState::new(AbrMode::Auto(None)));
    let peer: Arc<dyn Abr> = Arc::new(SeedPeer {
        state: Arc::clone(&state),
        variants: audio_variants_4tier(),
    });
    let handle = controller.register(&peer);
    controller.tick(handle.peer_id(), Instant::now());
    assert_eq!(state.current_variant_index(), 0);
    assert_eq!(state.pending_target(), None);
    drop(handle);
}

#[kithara::test(tokio)]
async fn lock_refcount_holds_across_record_bandwidth() {
    let settings = settings_fast();
    let controller =
        AbrController::with_estimator(settings, Arc::new(ThroughputEstimator::new()) as Arc<_>);

    struct LockedPeer {
        state: Arc<AbrState>,
        variants: Vec<VariantInfo>,
    }
    impl Abr for LockedPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
        fn variants(&self) -> Vec<VariantInfo> {
            self.variants.clone()
        }
    }
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(0))));
    state.lock();
    let peer: Arc<dyn Abr> = Arc::new(LockedPeer {
        state: Arc::clone(&state),
        variants: test_variants_3(),
    });
    let handle = controller.register(&peer);

    for _ in 0..20 {
        controller.record_bandwidth(
            handle.peer_id(),
            128 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
    }
    assert_eq!(state.current_variant_index(), 0);
    assert!(state.is_locked());

    state.unlock();
    assert!(!state.is_locked());
    drop(handle);
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Lock,
    PushBandwidth { bps: u64 },
    SetMode(ModeOp),
    Tick,
    Unlock,
}

#[derive(Clone, Copy, Debug)]
enum ModeOp {
    Auto,
    ManualOne,
    ManualTwo,
    ManualZero,
}

fn mode_from(op: ModeOp) -> AbrMode {
    match op {
        ModeOp::Auto => AbrMode::Auto(None),
        ModeOp::ManualOne => AbrMode::Manual(1),
        ModeOp::ManualTwo => AbrMode::Manual(2),
        ModeOp::ManualZero => AbrMode::Manual(0),
    }
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1_000u64..20_000_000u64).prop_map(|bps| Op::PushBandwidth { bps }),
        prop_oneof![
            Just(ModeOp::Auto),
            Just(ModeOp::ManualZero),
            Just(ModeOp::ManualOne),
            Just(ModeOp::ManualTwo),
        ]
        .prop_map(Op::SetMode),
        Just(Op::Lock),
        Just(Op::Unlock),
        Just(Op::Tick),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// SEEK-NO-SWITCH holds under any random op sequence, and manual mode
    /// always targets its requested index on the next decision.
    #[test]
    fn abr_state_respects_invariants(ops in proptest::collection::vec(arb_op(), 1..80)) {
        let variants = test_variants_3();
        let settings = settings_fast();
        let state = AbrState::new(AbrMode::Auto(Some(0)));

        let mut lock_depth = 0usize;
        let mut variant_at_lock = None;
        let mut current_bps: Option<u64> = None;
        let base_now = Instant::now();
        let mut tick = 0u64;

        for op in ops {
            tick = tick.saturating_add(1);
            let now = base_now + StdDuration::from_millis(tick * 10);

            match op {
                Op::PushBandwidth { bps } => {
                    current_bps = Some(bps);
                }
                Op::SetMode(mode_op) => {
                    state.set_mode(mode_from(mode_op));
                }
                Op::Lock => {
                    if lock_depth == 0 {
                        variant_at_lock = Some(state.current_variant_index());
                    }
                    state.lock();
                    lock_depth = lock_depth.saturating_add(1);
                }
                Op::Unlock => {
                    if lock_depth > 0 {
                        state.unlock();
                        lock_depth -= 1;
                        if lock_depth == 0 {
                            variant_at_lock = None;
                        }
                    }
                }
                Op::Tick => {
                    let view = view_with_bw(current_bps, &variants, &settings);
                    let d = state.decide(&view, now);
                    if d.did_change {
                        state.apply(&d, now);
                    }
                }
            }

            prop_assert_eq!(state.lock_count(), lock_depth);

            if lock_depth > 0 {
                prop_assert_eq!(
                    state.current_variant_index(),
                    variant_at_lock.expect("variant snapshot at first lock"),
                    "SEEK-NO-SWITCH violated while locked"
                );
            }

            if let AbrMode::Manual(idx) = state.mode()
                && idx < variants.len()
                && lock_depth == 0
            {
                let view = view_with_bw(current_bps, &variants, &settings);
                let d = state.decide(&view, now);
                if d.did_change {
                    state.apply(&d, now);
                }
                prop_assert_eq!(
                    state.current_variant_index(),
                    idx,
                    "manual(idx) must pin variant_index to idx after a tick"
                );
            }
        }
    }

    /// Monotonically increasing bandwidth (unlocked, Auto) never causes
    /// a down-switch.
    #[test]
    fn monotonic_bandwidth_never_down_switches(
        steps in proptest::collection::vec(200_000u64..10_000_000u64, 1..40),
    ) {
        let variants = test_variants_3();
        let settings = settings_fast();
        let state = AbrState::new(AbrMode::Auto(Some(0)));

        let mut cumulative_bps: u64 = 0;
        let mut prev_variant: Option<usize> = None;
        let base_now = Instant::now();

        for (tick, delta) in steps.into_iter().enumerate() {
            cumulative_bps = cumulative_bps.saturating_add(delta);
            let now = base_now + StdDuration::from_millis((tick as u64).saturating_add(1) * 10);
            let view = view_with_bw(Some(cumulative_bps), &variants, &settings);
            let d = state.decide(&view, now);
            if d.did_change && d.reason == AbrReason::DownSwitch {
                prop_assert!(
                    false,
                    "DownSwitch must not fire under monotonic bandwidth: bps={cumulative_bps}",
                );
            }
            if d.did_change {
                state.apply(&d, now);
            }
            let current = state.current_variant_index();
            if let Some(prev) = prev_variant {
                prop_assert!(
                    current >= prev,
                    "variant_index must not regress under monotonic bandwidth"
                );
            }
            prev_variant = Some(current);
        }
    }
}
