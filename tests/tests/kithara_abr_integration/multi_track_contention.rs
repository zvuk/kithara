use kithara::{
    self,
    abr::{AbrController, AbrMode, AbrState, ThroughputEstimator},
    events::{BandwidthSource, VariantIndex, VariantInfo},
    platform::{CancelToken, sync::Arc, time::Duration},
};

use super::common::{fast_settings, variants};

struct TestPeer {
    cancel: CancelToken,
    state: Arc<AbrState>,
    variants: Vec<VariantInfo>,
}

impl kithara::abr::Abr for TestPeer {
    fn cancel(&self) -> CancelToken {
        self.cancel.clone()
    }

    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.clone()
    }
    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }
}

fn new_peer(bitrates: &[u64]) -> (Arc<AbrState>, Arc<dyn kithara::abr::Abr>) {
    let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
    let peer: Arc<dyn kithara::abr::Abr> = Arc::new(TestPeer {
        cancel: CancelToken::never(),
        state: Arc::clone(&state),
        variants: variants(bitrates),
    });
    (state, peer)
}

#[kithara::test(tokio)]
async fn three_peers_maintain_independent_variant_indices() {
    let controller = AbrController::with_estimator(
        fast_settings(),
        Arc::new(ThroughputEstimator::new()) as Arc<_>,
    );

    let (s0, p0) = new_peer(&[300_000, 900_000]);
    let (s1, p1) = new_peer(&[400_000, 1_000_000, 3_000_000, 10_000_000]);
    let (s2, p2) = new_peer(&[512_000]);

    let h0 = controller.register(&p0);
    let h1 = controller.register(&p1);
    let h2 = controller.register(&p2);

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

    let v0 = s0.current_variant_index().get();
    let v1 = s1.current_variant_index().get();
    let v2 = s2.current_variant_index().get();

    assert!(v0 < 2, "peer0: variant out of range: {v0}");
    assert!(v1 < 4, "peer1: variant out of range: {v1}");
    assert_eq!(v2, 0, "peer2: only one variant available");

    drop((h0, h1, h2));
}
