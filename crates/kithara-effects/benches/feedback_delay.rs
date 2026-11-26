#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput};

mod support;

use support::feedback_delay::{CASES, GROUP, Prepared, observe, prepare, process_once};

/// One stereo `process` pass over the prepared delay. Returning the owner keeps
/// delay and buffer destruction outside the measured window.
#[metabench::benchmark(FEEDBACK_COMB, GROUP, "high_pass", gungraun_setup = prepare, gungraun_teardown = observe_timing)]
#[bench::integer_480(480.0)]
#[bench::integer_96k(96_000.0)]
#[bench::fractional_96k(96_000.5)]
fn feedback_delay_process(mut prepared: Prepared) -> Prepared {
    process_once(&mut prepared);
    prepared
}

fn observe_timing(prepared: Prepared) {
    observe(
        &prepared.identity,
        &serde_json::json!({
            "timing": {"policy": "system", "inner_backend": "system"}
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
                bencher.iter(|| process_once(&mut prepared));
            },
        );
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [FEEDBACK_COMB],
    allocator = std::alloc::System,
);
