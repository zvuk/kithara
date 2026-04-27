//! Property-based invariants for `AbrState`.
//!
//! Drives the state through random sequences of operations (PushBandwidth,
//! SetMode, Lock, Unlock, Tick) and checks the hard invariants called out
//! in the ABR projection §12 Tier 5 and §16.3:
//!
//!   1. `lock_count >= 0` (enforced structurally — `unlock` in isolation
//!      would panic; keep the refcount non-negative by pairing).
//!   2. Once locked, `current_variant_index()` does not change until the
//!      last lock is released — the SEEK-NO-SWITCH invariant.
//!   3. `AbrMode::Manual(idx)` immediately targets `idx` on the next
//!      `decide(..)` — manual override dominates bandwidth/buffer gates.
//!   4. Monotonically increasing bandwidth + unlocked auto mode only
//!      up-switches or stays on the current variant — never down-switches.
#![forbid(unsafe_code)]

use std::time::Duration as StdDuration;

use kithara_abr::{AbrMode, AbrSettings, AbrState, AbrView, test_variants_3};
use kithara_events::{AbrReason, AbrVariant};
use kithara_platform::time::{Duration, Instant};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
enum Op {
    PushBandwidth { bps: u64 },
    SetMode(ModeOp),
    Lock,
    Unlock,
    Tick,
}

#[derive(Clone, Copy, Debug)]
enum ModeOp {
    Auto,
    ManualZero,
    ManualOne,
    ManualTwo,
}

fn mode_from(op: ModeOp) -> AbrMode {
    match op {
        ModeOp::Auto => AbrMode::Auto(None),
        ModeOp::ManualZero => AbrMode::Manual(0),
        ModeOp::ManualOne => AbrMode::Manual(1),
        ModeOp::ManualTwo => AbrMode::Manual(2),
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

fn fast_settings() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    }
}

fn make_view<'a>(
    bps: Option<u64>,
    variants: &'a [AbrVariant],
    settings: &'a AbrSettings,
) -> AbrView<'a> {
    AbrView {
        estimate_bps: bps,
        buffer_ahead: None,
        bytes_downloaded: 10 * 1024 * 1024,
        variants,
        settings,
    }
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
        let settings = fast_settings();
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
                    let view = make_view(current_bps, &variants, &settings);
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
                let view = make_view(current_bps, &variants, &settings);
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
        let settings = fast_settings();
        let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(0)));

        let mut cumulative_bps: u64 = 0;
        let mut prev_variant: Option<usize> = None;
        let base_now = Instant::now();

        for (tick, delta) in steps.into_iter().enumerate() {
            cumulative_bps = cumulative_bps.saturating_add(delta);
            let now = base_now + StdDuration::from_millis((tick as u64).saturating_add(1) * 10);
            let view = make_view(Some(cumulative_bps), &variants, &settings);
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
