#![forbid(unsafe_code)]

use std::num::NonZeroU32;

use criterion::{BenchmarkId, Criterion, Throughput};
use kithara_effects::reverb::{Reverb, ReverbConfig};
use kithara_test_utils::bufpool::pools;

struct Prepared {
    effect: Reverb,
    input: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
}

fn prepare(frames: usize) -> Prepared {
    let input: Vec<f32> = [0.25, -0.125, 0.375, -0.5, 0.0625, 0.75, -0.25]
        .into_iter()
        .cycle()
        .take(frames)
        .collect();
    let left = vec![0.0; frames];
    let right = left.clone();
    let effect = Reverb::new(
        NonZeroU32::new(48_000).expect("bench rate"),
        NonZeroU32::new(48_000).expect("bench maximum rate"),
        ReverbConfig::default(),
        &pools(),
    )
    .expect("prepared network");
    Prepared {
        effect,
        input,
        left,
        right,
    }
}

fn process(prepared: &mut Prepared) {
    prepared
        .effect
        .process(
            [
                std::hint::black_box(&prepared.input),
                std::hint::black_box(&prepared.input),
            ],
            [
                std::hint::black_box(&mut prepared.left),
                std::hint::black_box(&mut prepared.right),
            ],
        )
        .expect("bench shape");
    std::hint::black_box((&prepared.left, &prepared.right));
}

#[metabench::benchmark(REVERB_STEREO, "reverb_process_48khz", "stereo", gungraun_setup = prepare)]
#[bench::frames_128(128_usize)]
#[bench::frames_512(512_usize)]
fn stereo_case(mut prepared: Prepared) -> Prepared {
    process(&mut prepared);
    prepared
}

fn scratch() {
    let mut scratch = [[0.0_f32; 128]; 8];
    std::hint::black_box(&mut scratch);
}

#[metabench::benchmark(REVERB_SCRATCH, "reverb", "scratch_zero_4kib")]
fn scratch_case() {
    scratch();
}

fn criterion_benchmarks(c: &mut Criterion) {
    {
        let mut group = c.benchmark_group("reverb_process_48khz");
        for frames in [128, 512] {
            group.throughput(Throughput::Elements(
                u64::try_from(frames).expect("bench frames"),
            ));
            let mut prepared = prepare(frames);
            group.bench_function(
                BenchmarkId::new("stereo", format!("frames_{frames}")),
                |b| {
                    b.iter(|| process(&mut prepared));
                },
            );
        }
        group.finish();
    }
    let mut scratch_group = c.benchmark_group("reverb");
    scratch_group.bench_function("scratch_zero_4kib", |b| b.iter(scratch));
    scratch_group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [REVERB_STEREO, REVERB_SCRATCH],
    allocator = std::alloc::System,
);
