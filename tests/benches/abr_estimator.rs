#![forbid(unsafe_code)]

use std::{hint::black_box, sync::Arc};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kithara_abr::{
    Abr, AbrController, AbrSettings, AbrState, BandwidthSource, Estimator, ThroughputEstimator,
};
use kithara_events::{VariantDuration, VariantIndex, VariantInfo};
use kithara_integration_tests::auto;
use kithara_platform::{CancelToken, time::Duration};

fn settings() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(2_000_000)
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build()
}

fn variant(idx: usize, bps: u64) -> VariantInfo {
    VariantInfo {
        variant_index: VariantIndex::new(idx),
        bandwidth_bps: Some(bps),
        duration: VariantDuration::Unknown,
        name: None,
        codecs: None,
        container: None,
    }
}

fn variants_4() -> Vec<VariantInfo> {
    vec![
        variant(0, 256_000),
        variant(1, 512_000),
        variant(2, 1_024_000),
        variant(3, 2_048_000),
    ]
}

struct BenchPeer {
    state: Arc<AbrState>,
    variants: Vec<VariantInfo>,
}

impl Abr for BenchPeer {
    fn variants(&self) -> Vec<VariantInfo> {
        self.variants.clone()
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }
}

fn bench_estimator_push_and_estimate(c: &mut Criterion) {
    let mut group = c.benchmark_group("abr_estimator_push_and_estimate");

    for (label, bytes, duration_ms) in [
        ("low_bitrate", 32_000u64, 250_u64),
        ("mid_bitrate", 96_000u64, 250_u64),
        ("high_bitrate", 256_000u64, 250_u64),
    ] {
        group.bench_with_input(
            BenchmarkId::new("32_samples", label),
            &(bytes, duration_ms),
            |b, &(bytes, duration_ms)| {
                b.iter(|| {
                    let estimator = ThroughputEstimator::new();
                    for _ in 0..32 {
                        estimator.push_sample(
                            bytes,
                            Duration::from_millis(duration_ms),
                            BandwidthSource::Network,
                        );
                    }
                    black_box(estimator.estimate_bps())
                });
            },
        );
    }

    group.finish();
}

fn bench_controller_record_bandwidth(c: &mut Criterion) {
    let mut group = c.benchmark_group("abr_controller_record_bandwidth");

    for (label, bytes, duration_ms) in [
        ("up_switch_pressure", 256_000u64, 250_u64),
        ("stable_mid", 96_000u64, 250_u64),
        ("down_switch_pressure", 32_000u64, 250_u64),
    ] {
        group.bench_with_input(
            BenchmarkId::new("record_bandwidth", label),
            &(bytes, duration_ms),
            |b, &(bytes, duration_ms)| {
                b.iter(|| {
                    let controller = AbrController::new(settings(), CancelToken::never());
                    let state = Arc::new(AbrState::new(auto(1)));
                    let peer: Arc<dyn Abr> = Arc::new(BenchPeer {
                        state: Arc::clone(&state),
                        variants: variants_4(),
                    });
                    let handle = controller.register(&peer);
                    for _ in 0..8 {
                        controller.record_bandwidth(
                            handle.peer_id(),
                            bytes,
                            Duration::from_millis(duration_ms),
                            BandwidthSource::Network,
                        );
                    }
                    black_box(state.current_variant_index())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_estimator_push_and_estimate,
    bench_controller_record_bandwidth
);
criterion_main!(benches);
