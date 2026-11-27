#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion};
use kithara::{
    abr::{
        Abr, AbrController, AbrSettings, AbrState, BandwidthSource, Estimator, ThroughputEstimator,
        VariantDuration, VariantIndex, VariantInfo,
    },
    platform::{CancelToken, sync::Arc, time::Duration},
};
use kithara_integration_tests::auto;

fn settings() -> AbrSettings {
    AbrSettings::builder()
        .initial_throughput_bps(Some(2_000_000))
        .min_switch_interval(Duration::ZERO)
        .min_buffer_for_up_switch(Duration::ZERO)
        .build()
}

const fn variant(idx: usize, bps: u64) -> VariantInfo {
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
    cancel: CancelToken,
    state: Arc<AbrState>,
    variants: Vec<VariantInfo>,
}

impl Abr for BenchPeer {
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

fn estimator_action(bytes: u64, duration_ms: u64) {
    let estimator = ThroughputEstimator::new();
    for _ in 0..32 {
        estimator.push_sample(
            bytes,
            Duration::from_millis(duration_ms),
            BandwidthSource::Network,
        );
    }
    std::hint::black_box(estimator.estimate_bps());
}

fn estimator_prepare(index: usize) -> (u64, u64) {
    [(32_000, 250), (96_000, 250), (256_000, 250)][index]
}

#[metabench::benchmark(ABR_ESTIMATOR, "abr_estimator_push_and_estimate", "32_samples", gungraun_setup = estimator_prepare)]
#[bench::low_bitrate(0_usize)]
#[bench::mid_bitrate(1_usize)]
#[bench::high_bitrate(2_usize)]
fn estimator_case(input: (u64, u64)) {
    estimator_action(input.0, input.1);
}

fn controller_action(bytes: u64, duration_ms: u64) {
    let controller = AbrController::new(settings());
    let state = Arc::new(AbrState::new(auto(1)));
    let peer: Arc<dyn Abr> = Arc::new(BenchPeer {
        cancel: CancelToken::never(),
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
    std::hint::black_box(state.current_variant_index());
}

fn controller_prepare(index: usize) -> (u64, u64) {
    [(256_000, 250), (96_000, 250), (32_000, 250)][index]
}

#[metabench::benchmark(ABR_CONTROLLER, "abr_controller_record_bandwidth", "record_bandwidth", gungraun_setup = controller_prepare)]
#[bench::up_switch_pressure(0_usize)]
#[bench::stable_mid(1_usize)]
#[bench::down_switch_pressure(2_usize)]
fn controller_case(input: (u64, u64)) {
    controller_action(input.0, input.1);
}

fn criterion_benchmarks(c: &mut Criterion) {
    {
        let mut group = c.benchmark_group("abr_estimator_push_and_estimate");
        for (index, label) in [(0, "low_bitrate"), (1, "mid_bitrate"), (2, "high_bitrate")] {
            let input = estimator_prepare(index);
            group.bench_with_input(
                BenchmarkId::new("32_samples", label),
                &input,
                |b, &(bytes, duration_ms)| {
                    b.iter(|| estimator_action(bytes, duration_ms));
                },
            );
        }
        group.finish();
    }

    {
        let mut group = c.benchmark_group("abr_controller_record_bandwidth");
        for (index, label) in [
            (0, "up_switch_pressure"),
            (1, "stable_mid"),
            (2, "down_switch_pressure"),
        ] {
            let input = controller_prepare(index);
            group.bench_with_input(
                BenchmarkId::new("record_bandwidth", label),
                &input,
                |b, &(bytes, duration_ms)| {
                    b.iter(|| controller_action(bytes, duration_ms));
                },
            );
        }
        group.finish();
    }
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [ABR_ESTIMATOR, ABR_CONTROLLER],
    allocator = std::alloc::System,
);
