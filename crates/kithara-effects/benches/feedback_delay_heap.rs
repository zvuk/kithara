#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput};
use kithara_test_utils::memory;

mod support;

use support::feedback_delay::{CASES, GROUP, Prepared, prepare, process_once};

/// One stereo `process` pass with the `Counting` global installed by `main!`.
/// The steady-state pass must neither hold new heap nor raise the live peak,
/// which pins the same no-allocation boundary as the allocation engine.
#[metabench::benchmark(FEEDBACK_COMB, GROUP, "high_pass", gungraun_setup = prepare)]
#[bench::integer_480(480.0)]
#[bench::integer_96k(96_000.0)]
#[bench::fractional_96k(96_000.5)]
fn feedback_delay_process_counted(mut prepared: Prepared) -> Prepared {
    let before = memory::live_bytes();
    memory::reset_peak();
    process_once(&mut prepared);
    assert_eq!(memory::live_bytes(), before, "process held new heap bytes");
    assert_eq!(memory::peak_bytes(), before, "process raised the live peak");
    prepared
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
