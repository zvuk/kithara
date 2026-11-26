#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput};
use kithara_test_utils::memory;

mod support;

use support::feedback_delay::{CASES, GROUP, Prepared, observe, prepare, process_once};

struct HeapPrepared {
    prepared: Prepared,
    before: usize,
    after: usize,
    peak: usize,
}

fn heap_prepare(frames: f64) -> HeapPrepared {
    HeapPrepared {
        prepared: prepare(frames),
        before: 0,
        after: 0,
        peak: 0,
    }
}

/// One stereo `process` pass with the `Counting` global installed by `main!`.
/// Observation serialization stays in teardown, outside the allocation probe.
#[metabench::benchmark(FEEDBACK_COMB, GROUP, "high_pass", gungraun_setup = heap_prepare, gungraun_teardown = observe_heap)]
#[bench::integer_480(480.0)]
#[bench::integer_96k(96_000.0)]
#[bench::fractional_96k(96_000.5)]
fn feedback_delay_process_counted(mut prepared: HeapPrepared) -> HeapPrepared {
    prepared.before = memory::live_bytes();
    memory::reset_peak();
    process_once(&mut prepared.prepared);
    prepared.after = memory::live_bytes();
    prepared.peak = memory::peak_bytes();
    assert_eq!(
        prepared.after, prepared.before,
        "process held new heap bytes"
    );
    assert_eq!(
        prepared.peak, prepared.before,
        "process raised the live peak"
    );
    prepared
}

fn observe_heap(prepared: HeapPrepared) {
    observe(
        &prepared.prepared.identity,
        &serde_json::json!({
            "heap": {
                "held_bytes": prepared.after.saturating_sub(prepared.before),
                "peak_bytes": prepared.peak.saturating_sub(prepared.before),
                "baseline_bytes": prepared.before,
                "window": "reset_peak; one process; live_bytes; peak_bytes",
                "policy": "counting",
                "inner_backend": "system",
            }
        }),
    );
    drop(prepared);
}

fn criterion_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group(FEEDBACK_COMB.group_name());
    group.throughput(Throughput::Elements(512));
    for (id, frames) in CASES {
        group.bench_function(
            BenchmarkId::new(FEEDBACK_COMB.benchmark_name(), id),
            |bencher| {
                let mut prepared = prepare(frames);
                bencher.iter(|| {
                    let before = memory::live_bytes();
                    memory::reset_peak();
                    process_once(&mut prepared);
                    assert_eq!(memory::live_bytes(), before, "process held new heap bytes");
                    assert_eq!(memory::peak_bytes(), before, "process raised the live peak");
                });
            },
        );
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [FEEDBACK_COMB],
    allocator = memory::Counting,
);
