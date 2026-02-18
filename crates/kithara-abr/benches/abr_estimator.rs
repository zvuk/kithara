#![forbid(unsafe_code)]

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use kithara_abr::{
    AbrController, AbrMode, AbrOptions, ThroughputEstimator, ThroughputSample,
    ThroughputSampleSource, Variant,
};
use web_time::Instant;

fn options() -> AbrOptions {
    AbrOptions {
        mode: AbrMode::Auto(Some(1)),
        variants: vec![
            Variant {
                variant_index: 0,
                bandwidth_bps: 256_000,
            },
            Variant {
                variant_index: 1,
                bandwidth_bps: 512_000,
            },
            Variant {
                variant_index: 2,
                bandwidth_bps: 1_024_000,
            },
            Variant {
                variant_index: 3,
                bandwidth_bps: 2_048_000,
            },
        ],
        ..AbrOptions::default()
    }
}

fn sample(bytes: u64, duration_ms: u64) -> ThroughputSample {
    ThroughputSample {
        bytes,
        duration: Duration::from_millis(duration_ms),
        at: Instant::now(),
        source: ThroughputSampleSource::Network,
        content_duration: None,
    }
}

fn bench_estimator_push_and_estimate(c: &mut Criterion) {
    let mut group = c.benchmark_group("abr_estimator_push_and_estimate");

    for (label, bytes, duration_ms) in [
        ("low_bitrate", 32_000, 250_u64),
        ("mid_bitrate", 96_000, 250_u64),
        ("high_bitrate", 256_000, 250_u64),
    ] {
        group.bench_with_input(
            BenchmarkId::new("32_samples", label),
            &(bytes, duration_ms),
            |b, &(bytes, duration_ms)| {
                b.iter(|| {
                    let cfg = options();
                    let mut estimator = ThroughputEstimator::new(&cfg);
                    for _ in 0..32 {
                        estimator.push_sample(sample(bytes, duration_ms));
                    }
                    black_box(estimator.estimate_bps())
                });
            },
        );
    }

    group.finish();
}

fn bench_controller_decide(c: &mut Criterion) {
    let mut group = c.benchmark_group("abr_controller_decide");

    for (label, bytes, duration_ms) in [
        ("up_switch_pressure", 256_000, 250_u64),
        ("stable_mid", 96_000, 250_u64),
        ("down_switch_pressure", 32_000, 250_u64),
    ] {
        group.bench_with_input(
            BenchmarkId::new("decide", label),
            &(bytes, duration_ms),
            |b, &(bytes, duration_ms)| {
                b.iter(|| {
                    let cfg = options();
                    let estimator = ThroughputEstimator::new(&cfg);
                    let mut controller = AbrController::with_estimator(cfg, estimator);
                    for _ in 0..8 {
                        controller.push_throughput_sample(sample(bytes, duration_ms));
                    }
                    black_box(controller.decide(Instant::now()))
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_estimator_push_and_estimate,
    bench_controller_decide
);
criterion_main!(benches);
