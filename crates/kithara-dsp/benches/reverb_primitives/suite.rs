use std::{hint::black_box, num::NonZeroUsize};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use fearless_simd::{Level, dispatch};
use kithara_dsp::{
    delay::{DelayDiffuser, DelayLine},
    filter::OnePoleLowPass,
};
use kithara_test_utils::bufpool::pools;

use crate::backend;

type Diffuse = fn(&mut [f32], &[f32], f32, &mut [f32]) -> usize;
type Blend = fn(&[f32], &[f32], f32, &mut [f32]) -> usize;

fn tone(frames: usize) -> Vec<f32> {
    [0.25, -0.125, 0.375, -0.5, 0.0625, 0.75, -0.25]
        .into_iter()
        .cycle()
        .take(frames)
        .collect()
}

fn scalar_diffuse(x: &mut [f32], d: &[f32], gain: f32, w: &mut [f32]) -> usize {
    for ((x, d), w) in x.iter_mut().zip(d).zip(w) {
        *w = gain.mul_add(*d, *x);
        *x = *d - *x;
    }
    x.len()
}

fn scalar_blend(x: &[f32], d: &[f32], fraction: f32, output: &mut [f32]) -> usize {
    for ((x, d), y) in x.iter().zip(d).zip(output.iter_mut()) {
        *y = fraction.mul_add(*d - *x, *x);
    }
    output.len()
}

fn kernels(c: &mut Criterion) {
    let diffuse: &[(&str, Diffuse)] = &[
        (
            "native",
            |x, d, g, w| dispatch!(Level::new(), s => backend::delay::diffuse_kernel(s,x,d,g,w)),
        ),
        (
            "fallback",
            |x, d, g, w| dispatch!(Level::fallback(), s => backend::delay::diffuse_kernel(s,x,d,g,w)),
        ),
        ("scalar", scalar_diffuse),
    ];
    let blend: &[(&str, Blend)] = &[
        (
            "native",
            |x, d, f, y| dispatch!(Level::new(), s => backend::delay::blend_kernel(s,x,d,f,y)),
        ),
        (
            "fallback",
            |x, d, f, y| dispatch!(Level::fallback(), s => backend::delay::blend_kernel(s,x,d,f,y)),
        ),
        ("scalar", scalar_blend),
    ];
    let mut group = c.benchmark_group("reverb_kernels");
    for frames in [128, 512] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("bench frames"),
        ));
        let x = tone(frames);
        let mut d = x.clone();
        d.rotate_left(3);
        let template = (x, d, vec![0.0; frames]);
        let mut expected = template.clone();
        scalar_diffuse(&mut expected.0, &expected.1, 0.5, &mut expected.2);
        for (name, kernel) in diffuse {
            let mut actual = template.clone();
            kernel(&mut actual.0, &actual.1, 0.5, &mut actual.2);
            assert_eq!(
                actual.0.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected.0.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(
                actual.2.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected.2.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
            group.bench_function(BenchmarkId::new(format!("diffuse/{name}"), frames), |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |(x, d, w)| {
                        black_box(kernel(
                            black_box(x),
                            black_box(d.as_slice()),
                            0.5,
                            black_box(w),
                        ));
                    },
                    BatchSize::NumIterations(64),
                );
            });
        }
        let mut expected = vec![0.0; frames];
        scalar_blend(&template.0, &template.1, 0.37, &mut expected);
        for (name, kernel) in blend {
            let mut output = vec![0.0; frames];
            kernel(&template.0, &template.1, 0.37, &mut output);
            assert_eq!(
                output.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
            group.bench_function(
                BenchmarkId::new(format!("delay_blend/{name}"), frames),
                |b| {
                    b.iter(|| {
                        black_box(kernel(
                            black_box(&template.0),
                            black_box(&template.1),
                            0.37,
                            black_box(&mut output),
                        ));
                    });
                },
            );
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
        for (name, level) in [("native", Level::new()), ("fallback", Level::fallback())] {
            let mut actual = template.clone();
            let mut state = [0.0; CHANNELS];
            let mut slices = actual.each_mut().map(|p| Some(p.as_mut_slice()));
            dispatch!(level, s => backend::one_pole::low_pass_kernel(s, 0.2, &mut state, &mut slices, frames));
            assert_eq!(
                actual
                    .iter()
                    .flatten()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .flatten()
                    .map(|x| x.to_bits())
                    .collect::<Vec<_>>()
            );
            assert_eq!(state.map(f32::to_bits), expected_state.map(f32::to_bits));
            state.fill(0.0);
            group.bench_function(BenchmarkId::new(name, frames), |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |planes| {
                        let mut slices = black_box(&mut *planes).each_mut().map(|p| Some(p.as_mut_slice()));
                        dispatch!(level, s => backend::one_pole::low_pass_kernel(s, 0.2, &mut state, &mut slices, frames));
                        black_box((planes, &state));
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
        let mut filter = OnePoleLowPass::<CHANNELS>::new(0.2).expect("bench coefficient");
        group.bench_function(BenchmarkId::new("process", frames), |b| {
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
