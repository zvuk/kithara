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

fn process<const WINDOW: bool>(filter: &mut Biquad, planes: &mut [Vec<f32>], frames: usize) {
    if WINDOW {
        if planes.len() == 1 {
            let plane = planes.first_mut().expect("one plane");
            filter
                .process_window(&mut [plane.as_mut_slice()], 0..frames)
                .expect("finite window");
        } else {
            let planes: &mut [Vec<f32>; 8] = planes.try_into().expect("eight planes");
            filter
                .process_window(&mut planes.each_mut().map(Vec::as_mut_slice), 0..frames)
                .expect("finite window");
        }
    } else {
        filter.reset();
        filter.process(planes, 0..frames).expect("matching planes");
    }
}

fn bench_filters<const WINDOW: bool>(c: &mut Criterion) {
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
    let mut group = c.benchmark_group(if WINDOW {
        "biquad_windows"
    } else {
        "biquad_planes"
    });
    for frames in [128, 512, 4_096] {
        let tone: Vec<_> = (0..frames)
            .map(|frame| (frame.to_f32().expect("bounded frame") * 0.0625).sin() * 0.5)
            .collect();
        let template = vec![tone; PLANES.get()];
        let mut expected = template.clone();
        let mut actual = template.clone();
        for (filter, plane) in individual.iter_mut().zip(&mut expected) {
            process::<WINDOW>(filter, std::slice::from_mut(plane), frames);
        }
        process::<WINDOW>(&mut together, &mut actual, frames);
        assert!(
            actual
                .iter()
                .flatten()
                .zip(expected.iter().flatten())
                .all(|(a, b)| if WINDOW {
                    (a - b).abs() <= 0.000_05
                } else {
                    a.to_bits() == b.to_bits()
                })
        );
        group.throughput(Throughput::Elements(
            u64::try_from(frames * PLANES.get()).expect("bounded samples"),
        ));
        group.bench_function(BenchmarkId::new("eight_individual", frames), |b| {
            b.iter_batched_ref(
                || template.clone(),
                |planes| {
                    for (filter, plane) in individual.iter_mut().zip(&mut *planes) {
                        process::<WINDOW>(filter, std::slice::from_mut(plane), frames);
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
                    process::<WINDOW>(&mut together, black_box(&mut *planes), frames);
                    black_box(planes);
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench(c: &mut Criterion) {
    bench_filters::<false>(c);
    bench_filters::<true>(c);
}

criterion_group!(benches, bench);
criterion_main!(benches);
