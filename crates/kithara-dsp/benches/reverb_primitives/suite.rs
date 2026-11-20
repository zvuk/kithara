use std::{hint::black_box, num::NonZeroUsize};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara_dsp::{
    delay::{DelayDiffuser, DelayLine},
    filter::OnePoleLowPass,
};
use kithara_test_utils::bufpool::pools;

fn tone(frames: usize) -> Vec<f32> {
    [0.25, -0.125, 0.375, -0.5, 0.0625, 0.75, -0.25]
        .into_iter()
        .cycle()
        .take(frames)
        .collect()
}

fn assert_bits(actual: &[f32], expected: &[f32]) {
    assert_eq!(
        actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
}

fn routed_diffuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("routes/delay_diffuse");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let input = tone(frames);
        let mut history = input.clone();
        history.rotate_left(3);
        let mut writes = vec![0.0; frames];
        for (name, kernel) in kithara_dsp::routes::delay_diffuse::routes() {
            group.bench_function(BenchmarkId::new(name, frames), |b| {
                b.iter_batched_ref(
                    || input.clone(),
                    |samples| {
                        kernel(
                            black_box(samples),
                            black_box(&history),
                            black_box(0.5),
                            black_box(&mut writes),
                        )
                    },
                    BatchSize::NumIterations(64),
                );
            });
        }
    }
    group.finish();
}

fn routed_blend(c: &mut Criterion) {
    let mut group = c.benchmark_group("routes/delay_blend");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let newer = tone(frames);
        let mut older = newer.clone();
        older.rotate_left(1);
        let mut output = vec![0.0; frames];
        for (name, kernel) in kithara_dsp::routes::delay_blend::routes() {
            group.bench_function(BenchmarkId::new(name, frames), |b| {
                b.iter(|| {
                    kernel(
                        black_box(&newer),
                        black_box(&older),
                        black_box(0.37),
                        black_box(&mut output),
                    )
                });
            });
        }
    }
    group.finish();
}

fn scalar_pole<const CHANNELS: usize>(
    planes: &mut [Vec<f32>; CHANNELS],
    state: &mut [f32; CHANNELS],
) {
    for (plane, memory) in planes.iter_mut().zip(state) {
        for sample in plane {
            *sample = 0.2_f32.mul_add(*memory, 0.8 * *sample);
            *memory = *sample;
        }
    }
}

fn one_pole<const CHANNELS: usize>(c: &mut Criterion) {
    let mut group = c.benchmark_group(format!("reverb_one_pole/{CHANNELS}ch"));
    for frames in [128, 512] {
        let template: [Vec<f32>; CHANNELS] = std::array::from_fn(|_| tone(frames));
        let mut expected = template.clone();
        let mut expected_state = [0.0; CHANNELS];
        scalar_pole(&mut expected, &mut expected_state);
        let mut actual = template.clone();
        let mut oracle = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
        oracle.process(&mut actual, 0..frames).expect("bench shape");
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_bits(actual, expected);
        }
        for name in ["native", "process"] {
            let mut filter = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
            group.bench_function(BenchmarkId::new(name, frames), |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |planes| {
                        filter
                            .process(black_box(planes), 0..frames)
                            .expect("bench shape");
                        black_box(planes);
                    },
                    BatchSize::NumIterations(64),
                );
            });
        }
        let mut scalar_state = [0.0; CHANNELS];
        group.bench_function(BenchmarkId::new("scalar", frames), |b| {
            b.iter_batched_ref(
                || template.clone(),
                |planes| {
                    scalar_pole(black_box(&mut *planes), &mut scalar_state);
                    black_box((planes, &scalar_state));
                },
                BatchSize::NumIterations(64),
            );
        });
    }
    group.finish();
}

fn blocks(c: &mut Criterion) {
    let mut group = c.benchmark_group("reverb_blocks");
    let region = pools();
    for frames in [128, 512] {
        let input = tone(frames);
        let mut diffuser = DelayDiffuser::<2>::new(
            DelayLine::new(
                NonZeroUsize::new(2).expect("stereo"),
                NonZeroUsize::new(127).expect("delay"),
                &region,
            )
            .expect("history"),
            0.5,
        )
        .expect("diffuser");
        group.bench_function(BenchmarkId::new("diffuser_process_2ch", frames), |b| {
            b.iter_batched_ref(
                || [input.clone(), input.clone()],
                |planes| {
                    diffuser
                        .process(black_box(planes), 0..frames)
                        .expect("shape");
                    black_box(planes);
                },
                BatchSize::NumIterations(64),
            );
        });
        let mut line = DelayLine::new(
            NonZeroUsize::MIN,
            NonZeroUsize::new(2048).expect("capacity"),
            &region,
        )
        .expect("line");
        let history = tone(2048);
        line.write(&[history.as_slice()], 0..2048)
            .expect("fill history");
        let mut output = vec![0.0; frames];
        group.bench_function(BenchmarkId::new("delay_integer_scalar", frames), |b| {
            b.iter(|| {
                for (sample, output) in black_box(history.get(1024..).expect("history span"))
                    .iter()
                    .zip(black_box(&mut output))
                {
                    *output = *sample;
                }
                black_box(&output);
            });
        });
        for (name, delay) in [("integer", 1024.0), ("fractional", 1024.37)] {
            let tap = line.tap(delay).expect("bench tap");
            group.bench_function(
                BenchmarkId::new(format!("delay_read/{name}"), frames),
                |b| {
                    b.iter(|| {
                        line.read(0, tap, black_box(&mut output))
                            .expect("read span");
                        black_box(&output);
                    });
                },
            );
        }
    }
    group.finish();
}

pub(crate) fn benchmarks(c: &mut Criterion) {
    routed_diffuse(c);
    routed_blend(c);
    blocks(c);
    one_pole::<2>(c);
    one_pole::<8>(c);
}
