#![forbid(unsafe_code)]

use std::{hint::black_box, num::NonZeroUsize};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fearless_simd::Level;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara_dsp::Accelerate;
use kithara_dsp::{Backend, Portable};

const SIZES: [usize; 6] = [64, 128, 256, 512, 1024, 4096];
const SIX: NonZeroUsize = NonZeroUsize::MIN.saturating_add(5);

fn layout<B: Backend>(c: &mut Criterion, name: &str, backend: &B) {
    let mut group = c.benchmark_group(format!("layout/{name}"));
    for frames in SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("frame count fits u64"),
        ));
        let (left, right) = (vec![0.25_f32; frames], vec![-0.25_f32; frames]);
        let mut pair = vec![0.0_f32; 2 * frames];
        group.bench_with_input(
            BenchmarkId::new("interleave_2ch", frames),
            &frames,
            |b, _| {
                b.iter(|| backend.interleave_pair(black_box(&left), black_box(&right), &mut pair));
            },
        );
        let (mut out_left, mut out_right) = (vec![0.0_f32; frames], vec![0.0_f32; frames]);
        group.bench_with_input(
            BenchmarkId::new("deinterleave_2ch", frames),
            &frames,
            |b, _| {
                b.iter(|| {
                    backend.deinterleave_pair(black_box(&pair), &mut out_left, &mut out_right)
                });
            },
        );
        let mut planes = vec![vec![0.25_f32; frames]; 6];
        let mut six = vec![0.0_f32; 6 * frames];
        group.bench_with_input(
            BenchmarkId::new("interleave_6ch", frames),
            &frames,
            |b, _| {
                b.iter(|| {
                    for (channel, plane) in planes.iter().enumerate() {
                        backend.scatter(black_box(plane), &mut six[channel..], SIX);
                    }
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("deinterleave_6ch", frames),
            &frames,
            |b, _| {
                b.iter(|| {
                    for (channel, plane) in planes.iter_mut().enumerate() {
                        backend.gather(black_box(&six[channel..]), SIX, plane);
                    }
                });
            },
        );
        let mut noisy = vec![f32::from_bits(1); frames];
        group.bench_with_input(BenchmarkId::new("sanitize", frames), &frames, |b, _| {
            b.iter(|| backend.sanitize(black_box(&mut noisy)));
        });
    }
    group.finish();
}

fn kernels(c: &mut Criterion) {
    layout(c, "portable-native", &Portable::default());
    layout(c, "portable-fallback", &Portable::new(Level::fallback()));
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    layout(c, "accelerate", &Accelerate);
}

criterion_group!(benches, kernels);
criterion_main!(benches);
