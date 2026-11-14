#![forbid(unsafe_code)]

use std::{hint::black_box, num::NonZeroU32};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_effects::reverb::{Reverb, ReverbConfig};
use kithara_test_utils::bufpool::pools;

fn reverb(c: &mut Criterion) {
    let mut group = c.benchmark_group("reverb_process_48khz");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let input: Vec<f32> = [0.25, -0.125, 0.375, -0.5, 0.0625, 0.75, -0.25]
            .into_iter()
            .cycle()
            .take(frames)
            .collect();
        let mut left = vec![0.0; frames];
        let mut right = left.clone();
        let mut effect = Reverb::new(
            NonZeroU32::new(48_000).expect("bench rate"),
            ReverbConfig::default(),
            &pools(),
        )
        .expect("prepared network");
        group.bench_function(BenchmarkId::new("stereo", frames), |b| {
            b.iter(|| {
                effect
                    .process(
                        [black_box(&input), black_box(&input)],
                        [black_box(&mut left), black_box(&mut right)],
                    )
                    .expect("bench shape");
                black_box((&left, &right));
            });
        });
    }
    group.finish();
}

fn scratch_zero(c: &mut Criterion) {
    c.bench_function("reverb_scratch_zero_4kib", |b| {
        b.iter(|| {
            let mut scratch = [[0.0_f32; 128]; 8];
            black_box(&mut scratch);
        });
    });
}

criterion_group!(benches, reverb, scratch_zero);
criterion_main!(benches);
