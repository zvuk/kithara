use std::{hint::black_box, num::NonZeroUsize};

use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, Throughput, measurement::WallTime,
};
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

fn scalar_diffuse(samples: &mut [f32], history: &mut [f32], gain: f32) {
    for (sample, delayed) in samples.iter_mut().zip(history) {
        let d = *delayed;
        *delayed = gain.mul_add(d, *sample);
        *sample = d - *sample;
    }
}

fn scalar_blend(newer: &[f32], older: &[f32], fraction: f32, output: &mut [f32]) {
    for ((newer, older), output) in newer.iter().zip(older).zip(output) {
        *output = fraction.mul_add(older - newer, *newer);
    }
}

fn diffuse(group: &mut BenchmarkGroup<'_, WallTime>, frames: usize) {
    let input = tone(frames);
    let mut history = input.clone();
    history.rotate_left(3);
    let region = pools();
    let mut line = DelayLine::new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(frames).expect("block capacity"),
        &region,
    )
    .expect("history");
    line.write(&[history.as_slice()], 0..frames)
        .expect("fill history");
    let mut diffuser = DelayDiffuser::<1>::new(line, 0.5).expect("diffuser");
    let mut actual = input.clone();
    let mut expected = input.clone();
    diffuser
        .process(&mut [actual.as_mut_slice()], 0..frames)
        .expect("shape");
    scalar_diffuse(&mut expected, &mut history, 0.5);
    assert_bits(&actual, &expected);
    group.bench_function(BenchmarkId::new("diffuse/native", frames), |b| {
        b.iter_batched_ref(
            || input.clone(),
            |samples| {
                diffuser
                    .process(&mut [black_box(samples.as_mut_slice())], 0..frames)
                    .expect("shape");
                black_box(samples);
            },
            BatchSize::NumIterations(64),
        );
    });
    group.bench_function(BenchmarkId::new("diffuse/scalar", frames), |b| {
        b.iter_batched_ref(
            || input.clone(),
            |samples| {
                scalar_diffuse(black_box(samples.as_mut_slice()), &mut history, 0.5);
                black_box(samples);
            },
            BatchSize::NumIterations(64),
        );
    });
}

fn blend(group: &mut BenchmarkGroup<'_, WallTime>, frames: usize) {
    let history = tone(2048);
    let region = pools();
    let mut line = DelayLine::new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(history.len()).expect("capacity"),
        &region,
    )
    .expect("line");
    line.write(&[history.as_slice()], 0..history.len())
        .expect("fill history");
    let fraction = 0.37;
    let tap = line.tap(1024.0 + f64::from(fraction)).expect("tap");
    let newer = history.get(1024..1024 + frames).expect("newer span");
    let older = history.get(1023..1023 + frames).expect("older span");
    let mut output = vec![0.0; frames];
    let mut expected = output.clone();
    line.read(0, tap, &mut output).expect("read span");
    scalar_blend(newer, older, fraction, &mut expected);
    assert_bits(&output, &expected);
    group.bench_function(BenchmarkId::new("delay_blend/native", frames), |b| {
        b.iter(|| {
            line.read(0, tap, black_box(&mut output))
                .expect("read span");
            black_box(&output);
        });
    });
    group.bench_function(BenchmarkId::new("delay_blend/scalar", frames), |b| {
        b.iter(|| {
            scalar_blend(
                black_box(newer),
                black_box(older),
                fraction,
                black_box(&mut output),
            );
            black_box(&output);
        });
    });
}

fn kernels(c: &mut Criterion) {
    let mut group = c.benchmark_group("reverb_kernels");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        diffuse(&mut group, frames);
        blend(&mut group, frames);
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
    kernels(c);
    blocks(c);
    one_pole::<2>(c);
    one_pole::<8>(c);
}
