use std::{sync::Arc, time::Duration as StdDuration};

use kithara_events::{AbrMode, AbrReason, AbrVariant, BandwidthSource, VariantDuration};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;
use proptest::prelude::*;

use super::{AbrDecision, AbrError, AbrState, AbrView};
use crate::{Abr, AbrController, AbrSettings, ThroughputEstimator};

/// Canonical 3-variant fixture used by every test in this module. Private
/// to the test module so it never leaks into the public API.
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

fn view_with_bw<'a>(
    bps: Option<u64>,
    variants: &'a [AbrVariant],
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
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    state.lock();
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(10_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert!(!d.changed);
    assert_eq!(d.reason, AbrReason::Locked);
}

#[kithara::test]
fn decide_many_samples_during_lock_never_switches() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
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
fn decide_warmup_blocks_switch() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        warmup_min_bytes: 128 * 1024,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    };
    let view = AbrView {
        estimate_bps: Some(10_000_000),
        buffer_ahead: None,
        bytes_downloaded: 1024,
        variants: &variants,
        settings: &settings,
    };
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::Warmup);
    assert!(!d.changed);
}

#[kithara::test]
fn decide_manual_mode_always_target() {
    let state = AbrState::new(test_variants_3(), AbrMode::Manual(2));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::ManualOverride);
    assert_eq!(d.target_variant_index, 2);
}

#[kithara::test]
fn decide_no_estimate_stays_put() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(1)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(None, &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::NoEstimate);
    assert!(!d.changed);
}

#[kithara::test]
fn decide_upswitch_when_bandwidth_allows() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(3_000_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::UpSwitch);
    assert_eq!(d.target_variant_index, 2);
}

#[kithara::test]
fn decide_downswitch_when_bandwidth_drops() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(2)));
    let variants = test_variants_3();
    let settings = settings_fast();
    let view = view_with_bw(Some(300_000), &variants, &settings);
    let d = state.decide(&view, Instant::now());
    assert_eq!(d.reason, AbrReason::DownSwitch);
    assert_eq!(d.target_variant_index, 0);
}

#[kithara::test]
fn decide_urgent_downswitch_when_buffer_low() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(2)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        urgent_downswitch_buffer: Duration::from_secs(5),
        down_hysteresis_ratio: 0.01, // high threshold — only urgent path fires
        min_switch_interval: Duration::ZERO,
        warmup_min_bytes: 0,
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
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        min_buffer_for_up_switch: Duration::from_secs(10),
        min_switch_interval: Duration::ZERO,
        warmup_min_bytes: 0,
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
    assert!(!d.changed);
}

#[kithara::test]
fn apply_updates_current_variant_and_timestamp() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    state.apply(
        &AbrDecision {
            reason: AbrReason::UpSwitch,
            changed: true,
            target_variant_index: 2,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), 2);
}

#[kithara::test]
fn apply_noop_when_same_variant() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(1)));
    state.apply(
        &AbrDecision {
            reason: AbrReason::AlreadyOptimal,
            changed: false,
            target_variant_index: 1,
        },
        Instant::now(),
    );
    assert_eq!(state.current_variant_index(), 1);
}

#[kithara::test]
fn set_mode_rejects_out_of_bounds_manual() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    let err = state.set_mode(AbrMode::Manual(10)).unwrap_err();
    assert_eq!(
        err,
        AbrError::VariantOutOfBounds {
            requested: 10,
            available: 3,
        }
    );
}

#[kithara::test]
fn lock_is_refcounted() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(None));
    state.lock();
    state.lock();
    assert!(state.is_locked());
    state.unlock();
    assert!(state.is_locked());
    state.unlock();
    assert!(!state.is_locked());
}

#[kithara::test]
fn min_switch_interval_prevents_oscillation() {
    let state = AbrState::new(test_variants_3(), AbrMode::Auto(Some(0)));
    let variants = test_variants_3();
    let settings = AbrSettings {
        min_switch_interval: Duration::from_secs(30),
        warmup_min_bytes: 0,
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
    assert!(d1.changed);
    state.apply(&d1, now);
    let d2 = state.decide(&view, now + Duration::from_secs(1));
    assert_eq!(d2.reason, AbrReason::MinInterval);
}

// ─────────────────────────────────────────────────────────────────────────
// SEEK-NO-SWITCH cases (previously in
// `tests/tests/kithara_abr_integration/seek_no_switch.rs`, moved into the
// crate so the only public consumer of `test_variants_3` is gone).
// ─────────────────────────────────────────────────────────────────────────

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
    let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(locked_variant)));
    state.lock();

    let now = Instant::now();
    for i in 0..50u64 {
        let v = view_with_bw(Some(base_bps + i * step_bps), &variants, &settings);
        let d = state.decide(&v, now + StdDuration::from_millis(i));
        assert!(!d.changed, "locked state decided to switch at iter {i}");
    }
    assert_eq!(state.current_variant_index(), locked_variant);
}

#[kithara::test(tokio)]
async fn lock_refcount_holds_across_record_bandwidth() {
    // Drive real `AbrController::record_bandwidth` with the peer locked
    // to confirm the published sample does not influence the variant
    // choice while the lock is held.
    let settings = settings_fast();
    let controller =
        AbrController::with_estimator(settings, Arc::new(ThroughputEstimator::new()) as Arc<_>);

    // Register a minimal fake peer via Abr trait, locked for the whole
    // test run.
    struct LockedPeer {
        state: Arc<AbrState>,
    }
    impl Abr for LockedPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
        fn variants(&self) -> Vec<AbrVariant> {
            self.state.variants_snapshot()
        }
    }
    let state = Arc::new(AbrState::new(test_variants_3(), AbrMode::Auto(Some(0))));
    state.lock();
    let peer: Arc<dyn Abr> = Arc::new(LockedPeer {
        state: Arc::clone(&state),
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

// ─────────────────────────────────────────────────────────────────────────
// Property-based invariants (previously `crates/kithara-abr/tests/state.rs`)
// ─────────────────────────────────────────────────────────────────────────

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
        let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(0)));

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
                    // Some Manual(idx) values are in-range; others bounce.
                    let _ = state.set_mode(mode_from(mode_op));
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
                    if d.changed {
                        state.apply(&d, now);
                    }
                }
            }

            // Invariant #1: lock count visible through the state matches
            // our accumulator.
            prop_assert_eq!(state.lock_count(), lock_depth);

            // Invariant #2 (SEEK-NO-SWITCH): while locked, the variant
            // index does not drift.
            if lock_depth > 0 {
                prop_assert_eq!(
                    state.current_variant_index(),
                    variant_at_lock.expect("variant snapshot at first lock"),
                    "SEEK-NO-SWITCH violated while locked"
                );
            }

            // Invariant #3 (manual-override): if last applied mode is
            // Manual(idx) with idx in range, a tick drives variant there.
            if let AbrMode::Manual(idx) = state.mode()
                && idx < variants.len()
                && lock_depth == 0
            {
                let view = view_with_bw(current_bps, &variants, &settings);
                let d = state.decide(&view, now);
                if d.changed {
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
        let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(0)));

        let mut cumulative_bps: u64 = 0;
        let mut prev_variant: Option<usize> = None;
        let base_now = Instant::now();

        for (tick, delta) in steps.into_iter().enumerate() {
            cumulative_bps = cumulative_bps.saturating_add(delta);
            let now = base_now + StdDuration::from_millis((tick as u64).saturating_add(1) * 10);
            let view = view_with_bw(Some(cumulative_bps), &variants, &settings);
            let d = state.decide(&view, now);
            if d.changed && d.reason == AbrReason::DownSwitch {
                prop_assert!(
                    false,
                    "DownSwitch must not fire under monotonic bandwidth: bps={cumulative_bps}",
                );
            }
            if d.changed {
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
