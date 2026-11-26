#![forbid(unsafe_code)]

use criterion::{BenchmarkId, Criterion, Throughput};

mod support;

use support::feedback_delay::{CASES, GROUP, Prepared, prepare, process_once};

/// One stereo `process` pass guarded against allocation and deallocation. The
/// `AllocDisabler` global installed by `main!` aborts the artifact if the pass
/// touches the allocator, so a completed run is the guard proof.
#[metabench::benchmark(FEEDBACK_COMB, GROUP, "high_pass", gungraun_setup = prepare)]
#[bench::integer_480(480.0)]
#[bench::integer_96k(96_000.0)]
#[bench::fractional_96k(96_000.5)]
fn feedback_delay_process_guarded(mut prepared: Prepared) -> Prepared {
    assert_no_alloc::assert_no_alloc(|| process_once(&mut prepared));
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
                bencher.iter(|| assert_no_alloc::assert_no_alloc(|| process_once(&mut prepared)));
            },
        );
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [FEEDBACK_COMB],
    allocator = assert_no_alloc::AllocDisabler,
);
