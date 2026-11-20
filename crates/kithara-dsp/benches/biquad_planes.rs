#![forbid(unsafe_code)]

use std::{hint::black_box, num::NonZeroUsize};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_dsp::filter::{Biquad, Coefficients, Hertz, Type};
use num_traits::ToPrimitive;

const PLANES: NonZeroUsize = NonZeroUsize::MIN.saturating_add(7);
const SECTIONS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

fn design(plane: usize) -> Coefficients<f64> {
    let (kind, cutoff) = [
        (Type::LowPass, 155.0),
        (Type::HighPass, 155.0),
        (Type::HighPass, 1_040.0),
        (Type::HighPass, 6_950.0),
    ][plane % 4];
    Coefficients::from_params(
        kind,
        Hertz::from_hz(44_100.0).expect("positive rate"),
        Hertz::from_hz(cutoff).expect("positive cutoff"),
        std::f64::consts::FRAC_1_SQRT_2,
    )
    .expect("valid band design")
}

fn bench(c: &mut Criterion) {
    let mut together = Biquad::new(PLANES, SECTIONS).expect("eight planes");
    let mut individual: Vec<_> = (0..PLANES.get())
        .map(|plane| {
            let mut single = Biquad::new(NonZeroUsize::MIN, SECTIONS).expect("one plane");
            for section in 0..SECTIONS.get() {
                let coefficients = design(plane);
                single.retune(section, coefficients).expect("valid section");
                together
                    .retune_plane(plane, section, coefficients)
                    .expect("valid plane");
            }
            single
        })
        .collect();
    let mut group = c.benchmark_group("biquad_planes");
    for frames in [128, 512, 4_096] {
        let tone: Vec<_> = (0..frames)
            .map(|frame| (frame.to_f32().expect("bounded frame") * 0.0625).sin() * 0.5)
            .collect();
        let template = vec![tone; PLANES.get()];
        let mut expected = template.clone();
        let mut actual = template.clone();
        for (filter, plane) in individual.iter_mut().zip(&mut expected) {
            filter.reset();
            filter
                .process(std::slice::from_mut(plane), 0..frames)
                .expect("one plane");
        }
        together.reset();
        together
            .process(&mut actual, 0..frames)
            .expect("eight planes");
        assert!(
            actual
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        group.throughput(Throughput::Elements(
            u64::try_from(frames * PLANES.get()).expect("bounded samples"),
        ));
        group.bench_function(BenchmarkId::new("eight_individual", frames), |b| {
            b.iter_batched_ref(
                || template.clone(),
                |planes| {
                    for (filter, plane) in individual.iter_mut().zip(&mut *planes) {
                        filter.reset();
                        filter
                            .process(std::slice::from_mut(plane), 0..frames)
                            .expect("one plane");
                    }
                    black_box(planes);
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function(BenchmarkId::new("eight_together", frames), |b| {
            b.iter_batched_ref(
                || template.clone(),
                |planes| {
                    together.reset();
                    together
                        .process(black_box(&mut *planes), 0..frames)
                        .expect("eight planes");
                    black_box(planes);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
