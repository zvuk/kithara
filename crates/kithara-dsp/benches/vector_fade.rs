#![forbid(unsafe_code)]

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_dsp::fade::{self, FadeCurve};
use num_traits::ToPrimitive;

mod consts {
    pub(super) const SIZES: [usize; 6] = [64, 128, 256, 512, 1024, 4096];
    pub(super) const TONE_STEP: f32 = 0.05;
}

fn tone(len: usize) -> Vec<f32> {
    std::iter::successors(Some(0.0_f32), |phase| Some(phase + consts::TONE_STEP))
        .map(f32::sin)
        .take(len)
        .collect()
}

fn vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector");
    for size in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(size).expect("size fits u64"),
        ));
        let samples = tone(size);
        // Every in-place kernel keeps its buffer finite and away from
        // subnormals over any number of iterations.
        let mut output = tone(size);
        group.bench_with_input(BenchmarkId::new("ramp", size), &size, |b, _| {
            b.iter(|| kithara_dsp::ramp(black_box(&mut output), 0.0, 1.0));
        });
        group.bench_with_input(BenchmarkId::new("div", size), &size, |b, _| {
            b.iter(|| kithara_dsp::div(black_box(&mut output), -1.0));
        });
        group.bench_with_input(BenchmarkId::new("affine", size), &size, |b, _| {
            b.iter(|| kithara_dsp::affine(black_box(&mut output), 0.5, 0.25));
        });
        group.bench_with_input(BenchmarkId::new("sub", size), &size, |b, _| {
            b.iter(|| kithara_dsp::sub(black_box(&mut output), black_box(&samples)));
        });
        group.bench_with_input(BenchmarkId::new("scale_add", size), &size, |b, _| {
            b.iter(|| kithara_dsp::scale_add(black_box(&samples), 0.5, black_box(&mut output)));
        });
        group.bench_with_input(BenchmarkId::new("mul_add", size), &size, |b, _| {
            b.iter(|| {
                kithara_dsp::mul_add(
                    black_box(&samples),
                    black_box(&samples),
                    black_box(&mut output),
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("add", size), &size, |b, _| {
            b.iter(|| kithara_dsp::add(black_box(&samples), black_box(&mut output)));
        });
        let gains = vec![-1.0; size];
        group.bench_with_input(
            BenchmarkId::new("multiply_in_place", size),
            &size,
            |b, _| {
                b.iter(|| {
                    kithara_dsp::multiply_in_place(black_box(&mut output), black_box(&gains))
                });
            },
        );
    }
    group.finish();
}

fn fade_gains(c: &mut Criterion) {
    let mut group = c.benchmark_group("fade_gains");
    for size in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(size).expect("size fits u64"),
        ));
        let mut positions = vec![0.0_f32; size];
        kithara_dsp::ramp(&mut positions, 0.0, 1.0);
        kithara_dsp::div(&mut positions, size.to_f32().expect("size is finite"));
        let mut outgoing = vec![0.0_f32; size];
        let mut incoming = vec![0.0_f32; size];
        for curve in [
            FadeCurve::EqualPower3dB,
            FadeCurve::EqualPower6dB,
            FadeCurve::SquareRoot,
            FadeCurve::Linear,
        ] {
            group.bench_with_input(
                BenchmarkId::new(format!("{curve:?}"), size),
                &size,
                |b, _| {
                    b.iter(|| {
                        fade::gains(
                            curve,
                            black_box(&positions),
                            black_box(&mut outgoing),
                            black_box(&mut incoming),
                        )
                    });
                },
            );
        }
    }
    group.finish();
}

fn detector(c: &mut Criterion) {
    let mut group = c.benchmark_group("fir_history");
    group.sample_size(20);
    for frames in [128, 256, 257, 600] {
        for (label, amplitude) in [("quiet", 0.125), ("overloaded", 2.0)] {
            let input: Vec<f32> = tone(frames)
                .into_iter()
                .map(|sample| sample * amplitude)
                .collect();
            let detector = kithara_dsp::filter::Fir::<17, 7, 1, 9>::new([[0.125; 17]; 7]);
            let mut output = vec![0.0; frames];
            group.bench_function(BenchmarkId::new(label, frames), |b| {
                b.iter_batched_ref(
                    || (detector.clone(), input.clone()),
                    |(detector, input)| {
                        detector.begin_block(&[input.as_mut_slice()], 0.98);
                        detector.process(
                            0,
                            black_box(input.as_slice()),
                            black_box(input.as_slice()),
                            black_box(output.as_mut_slice()),
                        );
                        black_box(&output);
                    },
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

fn envelope(c: &mut Criterion) {
    let mut group = c.benchmark_group("attack_release");
    group.sample_size(20);
    for frames in [128, 256, 257, 600] {
        let envelope = kithara_dsp::envelope::AttackRelease::new(0.999_583_4);
        let targets: Vec<f32> = [1.0, 0.25, 0.8, 0.99]
            .into_iter()
            .cycle()
            .take(frames)
            .collect();
        group.bench_function(BenchmarkId::from_parameter(frames), |b| {
            b.iter_batched_ref(
                || (envelope.clone(), targets.clone()),
                |(envelope, targets)| {
                    envelope.process(black_box(targets.as_mut_slice()));
                    black_box(targets.as_slice());
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, vector, fade_gains, detector, envelope);
criterion_main!(benches);
