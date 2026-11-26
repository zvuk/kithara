#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
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

struct Prepared {
    together: Biquad,
    individual: Vec<Biquad>,
    template: Vec<Vec<f32>>,
    planes: Vec<Vec<f32>>,
}

fn prepare(frames: usize) -> Prepared {
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
    let planes = template.clone();
    Prepared {
        together,
        individual,
        template,
        planes,
    }
}

fn process_individual(filters: &mut [Biquad], planes: &mut [Vec<f32>], frames: usize) {
    for (filter, plane) in filters.iter_mut().zip(&mut *planes) {
        filter.reset();
        filter
            .process(std::slice::from_mut(plane), 0..frames)
            .expect("one plane");
    }
    std::hint::black_box(planes);
}

fn process_together(filter: &mut Biquad, planes: &mut [Vec<f32>], frames: usize) {
    filter.reset();
    filter
        .process(std::hint::black_box(&mut *planes), 0..frames)
        .expect("eight planes");
    std::hint::black_box(planes);
}

#[metabench::benchmark(BIQUAD_INDIVIDUAL, "biquad_planes", "eight_individual", gungraun_setup = prepare)]
#[bench::frames_128(128_usize)]
#[bench::frames_512(512_usize)]
#[bench::frames_4096(4096_usize)]
fn individual_case(mut prepared: Prepared) -> Prepared {
    let frames = prepared.planes[0].len();
    process_individual(&mut prepared.individual, &mut prepared.planes, frames);
    prepared
}

#[metabench::benchmark(BIQUAD_TOGETHER, "biquad_planes", "eight_together", gungraun_setup = prepare)]
#[bench::frames_128(128_usize)]
#[bench::frames_512(512_usize)]
#[bench::frames_4096(4096_usize)]
fn together_case(mut prepared: Prepared) -> Prepared {
    let frames = prepared.planes[0].len();
    process_together(&mut prepared.together, &mut prepared.planes, frames);
    prepared
}

fn criterion_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("biquad_planes");
    for frames in [128, 512, 4_096] {
        let Prepared {
            mut together,
            mut individual,
            template,
            ..
        } = prepare(frames);
        group.throughput(Throughput::Elements(
            u64::try_from(frames * PLANES.get()).expect("bounded samples"),
        ));
        group.bench_function(
            BenchmarkId::new("eight_individual", format!("frames_{frames}")),
            |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |planes| process_individual(&mut individual, planes, frames),
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_function(
            BenchmarkId::new("eight_together", format!("frames_{frames}")),
            |b| {
                b.iter_batched_ref(
                    || template.clone(),
                    |planes| process_together(&mut together, planes, frames),
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [BIQUAD_INDIVIDUAL, BIQUAD_TOGETHER],
    allocator = std::alloc::System,
);
