#![forbid(unsafe_code)]

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use kithara_abr::{
    Abr, AbrController, AbrMode, AbrSettings, AbrState, BandwidthSource, Estimator,
    ThroughputEstimator,
};
use kithara_events::{AbrProgressSnapshot, AbrVariant, VariantDuration};
use kithara_platform::time::Duration;

fn settings() -> AbrSettings {
    AbrSettings {
        warmup_min_bytes: 0,
        min_switch_interval: Duration::ZERO,
        min_buffer_for_up_switch: Duration::ZERO,
        min_throughput_record_ms: 0,
        ..AbrSettings::default()
    }
}

fn variants_4() -> Vec<AbrVariant> {
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
        AbrVariant {
            variant_index: 3,
            bandwidth_bps: 2_048_000,
            duration: VariantDuration::Unknown,
        },
    ]
}

struct BenchPeer {
    state: Arc<AbrState>,
}

impl Abr for BenchPeer {
    fn variants(&self) -> Vec<AbrVariant> {
        self.state.variants_snapshot()
    }

    fn state(&self) -> Option<Arc<AbrState>> {
        Some(Arc::clone(&self.state))
    }

    fn progress(&self) -> Option<AbrProgressSnapshot> {
        None
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
                    let controller = AbrController::new(settings());
                    let state = Arc::new(AbrState::new(variants_4(), AbrMode::Auto(Some(1))));
                    let peer: Arc<dyn Abr> = Arc::new(BenchPeer {
                        state: Arc::clone(&state),
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
