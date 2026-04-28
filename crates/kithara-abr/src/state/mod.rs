//! Per-peer ABR state, decision logic, and supporting types.
//!
//! Layout:
//! - `core` — `AbrState` struct with accessors and commands (the bag of
//!   atomics + a mutex for the variant list).
//! - `decision` — pure decision function (`evaluate`) that turns an
//!   `(AbrState, AbrView, Instant)` triple into an `AbrDecision`. Refactored
//!   from a heterogeneous guard cascade into parallel compute + a single
//!   tuple-match — see file-level docs there for the rationale.
//! - `view` — `AbrView<'a>` snapshot of the inputs the decision needs.
//! - `error` — `AbrError` for the few state mutations that can fail.

mod core;
mod decision;
mod error;
mod view;

pub use core::AbrState;

pub use decision::AbrDecision;
pub use error::AbrError;
#[cfg(any(test, feature = "internal"))]
use kithara_events::{AbrVariant, VariantDuration};
pub use view::AbrView;

/// Build a canonical set of 3 variants for tests and fixtures.
#[cfg(any(test, feature = "internal"))]
#[must_use]
pub fn test_variants_3() -> Vec<AbrVariant> {
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

#[cfg(test)]
mod tests {
    use kithara_events::{AbrMode, AbrReason};
    use kithara_platform::time::{Duration, Instant};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::controller::AbrSettings;

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
                target_variant_index: 2,
                reason: AbrReason::UpSwitch,
                changed: true,
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
                target_variant_index: 1,
                reason: AbrReason::AlreadyOptimal,
                changed: false,
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
}
