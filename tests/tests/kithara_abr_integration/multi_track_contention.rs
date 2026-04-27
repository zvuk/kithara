//! Three parallel peers feed their own variant lists into a shared
//! `AbrController`; bandwidth records on one peer must never bleed into
//! another peer's variant index.

use std::sync::Arc;

use kithara_abr::{AbrController, AbrMode, AbrSettings, AbrState, ThroughputEstimator};
use kithara_events::{AbrVariant, BandwidthSource, VariantDuration};
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;

fn settings_fast() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        ..AbrSettings::default()
    }
}

struct TestPeer {
    state: Arc<AbrState>,
}

impl kithara_abr::Abr for TestPeer {
    fn variants(&self) -> Vec<AbrVariant> {
        self.state.variants_snapshot()
    }
    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }
}

fn variants(bitrates: &[u64]) -> Vec<AbrVariant> {
    bitrates
        .iter()
        .enumerate()
        .map(|(i, bps)| AbrVariant {
            variant_index: i,
            bandwidth_bps: *bps,
            duration: VariantDuration::Unknown,
        })
        .collect()
}

fn new_peer(bitrates: &[u64]) -> (Arc<AbrState>, Arc<dyn kithara_abr::Abr>) {
    let state = Arc::new(AbrState::new(variants(bitrates), AbrMode::Auto(Some(0))));
    let peer: Arc<dyn kithara_abr::Abr> = Arc::new(TestPeer {
        state: Arc::clone(&state),
    });
    (state, peer)
}

#[kithara::test(tokio)]
async fn three_peers_maintain_independent_variant_indices() {
    let controller = AbrController::with_estimator(
        settings_fast(),
        Arc::new(ThroughputEstimator::new()) as Arc<_>,
    );

    let (s0, p0) = new_peer(&[300_000, 900_000]);
    let (s1, p1) = new_peer(&[400_000, 1_000_000, 3_000_000, 10_000_000]);
    let (s2, p2) = new_peer(&[512_000]);

    let h0 = controller.register(&p0);
    let h1 = controller.register(&p1);
    let h2 = controller.register(&p2);

    // Feed unrelated bandwidth samples; peer_id routing must isolate
    // each peer's state. With fast settings we only verify that
    // per-peer state does not see a variant index that doesn't exist
    // on that peer.
    for _ in 0..10 {
        controller.record_bandwidth(
            h0.peer_id(),
            32 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
        controller.record_bandwidth(
            h1.peer_id(),
            256 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
        controller.record_bandwidth(
            h2.peer_id(),
            64 * 1024,
            Duration::from_millis(50),
            BandwidthSource::Network,
        );
    }

    let v0 = s0.current_variant_index();
    let v1 = s1.current_variant_index();
    let v2 = s2.current_variant_index();

    assert!(v0 < 2, "peer0: variant out of range: {v0}");
    assert!(v1 < 4, "peer1: variant out of range: {v1}");
    assert_eq!(v2, 0, "peer2: only one variant available");

    drop((h0, h1, h2));
}
