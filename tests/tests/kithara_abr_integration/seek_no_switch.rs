//! SEEK-NO-SWITCH invariant: `AbrState` must not change variant while
//! locked, regardless of how many bandwidth samples arrive or how the
//! throughput estimate swings.

use std::{sync::Arc, time::Duration as StdDuration};

use kithara_abr::{
    AbrController, AbrMode, AbrSettings, AbrState, AbrView, ThroughputEstimator, test_variants_3,
};
use kithara_events::{AbrVariant, BandwidthSource};
use kithara_platform::time::{Duration, Instant};
use kithara_test_utils::kithara;

fn settings_fast() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    }
}

fn view<'a>(bps: Option<u64>, variants: &'a [AbrVariant], s: &'a AbrSettings) -> AbrView<'a> {
    AbrView {
        estimate_bps: bps,
        buffer_ahead: None,
        bytes_downloaded: 4 * 1024 * 1024,
        variants,
        settings: s,
    }
}

#[kithara::test]
fn locked_state_rejects_up_switch_under_high_bandwidth() {
    let variants = test_variants_3();
    let settings = settings_fast();
    let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(0)));
    state.lock();

    let now = Instant::now();
    for i in 0..50u64 {
        let v = view(Some(20_000_000 + i * 100_000), &variants, &settings);
        let d = state.decide(&v, now + StdDuration::from_millis(i));
        assert!(!d.changed, "locked state decided to switch at iter {i}");
    }
    assert_eq!(state.current_variant_index(), 0);
}

#[kithara::test]
fn locked_state_rejects_down_switch_under_low_bandwidth() {
    let variants = test_variants_3();
    let settings = settings_fast();
    let state = AbrState::new(variants.clone(), AbrMode::Auto(Some(2)));
    state.lock();

    let now = Instant::now();
    for i in 0..50u64 {
        let v = view(Some(10_000 + i), &variants, &settings);
        let d = state.decide(&v, now + StdDuration::from_millis(i));
        assert!(!d.changed, "locked state decided to switch at iter {i}");
    }
    assert_eq!(state.current_variant_index(), 2);
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
    impl kithara_abr::Abr for LockedPeer {
        fn variants(&self) -> Vec<AbrVariant> {
            self.state.variants_snapshot()
        }
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
    }
    let state = Arc::new(AbrState::new(test_variants_3(), AbrMode::Auto(Some(0))));
    state.lock();
    let peer: Arc<dyn kithara_abr::Abr> = Arc::new(LockedPeer {
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
