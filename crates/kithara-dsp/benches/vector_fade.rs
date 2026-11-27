#![forbid(unsafe_code)]

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
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
        group.bench_with_input(
            BenchmarkId::new("ramp", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| kithara_dsp::ramp(std::hint::black_box(&mut output), 0.0, 1.0));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("div", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| kithara_dsp::div(std::hint::black_box(&mut output), -1.0));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("scale_in_place", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| {
                    let samples: &mut Vec<f32> = std::hint::black_box(&mut output);
                    kithara_dsp::scale_in_place(samples, -1.0)
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("affine", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| kithara_dsp::affine(std::hint::black_box(&mut output), 0.5, 0.25));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("sub", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| {
                    kithara_dsp::sub(
                        std::hint::black_box(&mut output),
                        std::hint::black_box(&samples),
                    )
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("scale_add", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| {
                    kithara_dsp::scale_add(
                        std::hint::black_box(&samples),
                        0.5,
                        std::hint::black_box(&mut output),
                    )
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("mul_add", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| {
                    kithara_dsp::mul_add(
                        std::hint::black_box(&samples),
                        std::hint::black_box(&samples),
                        std::hint::black_box(&mut output),
                    )
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("add", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter(|| {
                    kithara_dsp::add(
                        std::hint::black_box(&samples),
                        std::hint::black_box(&mut output),
                    )
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
        for (label, curve) in [
            ("equal_power_3db", FadeCurve::EqualPower3dB),
            ("equal_power_6db", FadeCurve::EqualPower6dB),
            ("square_root", FadeCurve::SquareRoot),
            ("linear", FadeCurve::Linear),
        ] {
            group.bench_with_input(
                BenchmarkId::new("gains", format!("{label}_size_{size}")),
                &size,
                |b, _| {
                    b.iter(|| {
                        fade::gains(
                            curve,
                            std::hint::black_box(&positions),
                            std::hint::black_box(&mut outgoing),
                            std::hint::black_box(&mut incoming),
                        )
                    });
                },
            );
        }
    }
    group.finish();
}

fn render_products(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_products");
    for size in [1, 128, 257, 1023] {
        let samples: Vec<_> = tone(size)
            .into_iter()
            .map(|sample| sample + 0.125)
            .collect();
        let gains = vec![0.7; size];
        group.bench_with_input(
            BenchmarkId::new("scalar_scale", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter_batched_ref(
                    || samples.clone(),
                    |samples| {
                        let gain = std::hint::black_box(0.7);
                        for sample in std::hint::black_box(samples) {
                            *sample *= gain;
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("scale", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter_batched_ref(
                    || samples.clone(),
                    |samples| {
                        let samples: &mut Vec<f32> = std::hint::black_box(samples);
                        let gain = std::hint::black_box(0.7);
                        kithara_dsp::scale_in_place(samples, gain)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("scalar_product", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter_batched_ref(
                    || samples.clone(),
                    |samples| {
                        for (sample, gain) in std::hint::black_box(samples)
                            .iter_mut()
                            .zip(std::hint::black_box(&gains))
                        {
                            *sample *= *gain;
                        }
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("product", format!("size_{size}")),
            &size,
            |b, _| {
                b.iter_batched_ref(
                    || samples.clone(),
                    |samples| {
                        let samples: &mut Vec<f32> = std::hint::black_box(samples);
                        let gains: &Vec<f32> = std::hint::black_box(&gains);
                        kithara_dsp::multiply_in_place(samples, gains)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
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
            group.bench_function(BenchmarkId::new(label, format!("size_{frames}")), |b| {
                b.iter_batched_ref(
                    || (detector.clone(), input.clone()),
                    |(detector, input)| {
                        detector.begin_block(&[input.as_mut_slice()], 0.98);
                        detector.process(
                            0,
                            std::hint::black_box(input.as_slice()),
                            std::hint::black_box(input.as_slice()),
                            std::hint::black_box(output.as_mut_slice()),
                        );
                        std::hint::black_box(&output);
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
        group.bench_function(BenchmarkId::new("process", format!("size_{frames}")), |b| {
            b.iter_batched_ref(
                || (envelope.clone(), targets.clone()),
                |(envelope, targets)| {
                    envelope.process(std::hint::black_box(targets.as_mut_slice()));
                    std::hint::black_box(targets.as_slice());
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn criterion_benchmarks(c: &mut Criterion) {
    vector(c);
    fade_gains(c);
    render_products(c);
    detector(c);
    envelope(c);
}

struct VectorPrepared {
    samples: Vec<f32>,
    output: Vec<f32>,
}

fn vector_prepare(size: usize) -> VectorPrepared {
    VectorPrepared {
        samples: tone(size),
        output: tone(size),
    }
}

#[metabench::benchmark(VECTOR_RAMP, "vector", "ramp", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_ramp(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::ramp(std::hint::black_box(&mut prepared.output), 0.0, 1.0);
    prepared
}

#[metabench::benchmark(VECTOR_DIV, "vector", "div", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_div(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::div(std::hint::black_box(&mut prepared.output), -1.0);
    prepared
}

#[metabench::benchmark(VECTOR_SCALE_IN_PLACE, "vector", "scale_in_place", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_scale_in_place(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::scale_in_place(std::hint::black_box(&mut prepared.output), -1.0);
    prepared
}

#[metabench::benchmark(VECTOR_AFFINE, "vector", "affine", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_affine(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::affine(std::hint::black_box(&mut prepared.output), 0.5, 0.25);
    prepared
}

#[metabench::benchmark(VECTOR_SUB, "vector", "sub", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_sub(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::sub(
        std::hint::black_box(&mut prepared.output),
        std::hint::black_box(&prepared.samples),
    );
    prepared
}

#[metabench::benchmark(VECTOR_SCALE_ADD, "vector", "scale_add", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_scale_add(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::scale_add(
        std::hint::black_box(&prepared.samples),
        0.5,
        std::hint::black_box(&mut prepared.output),
    );
    prepared
}

#[metabench::benchmark(VECTOR_MUL_ADD, "vector", "mul_add", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_mul_add(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::mul_add(
        std::hint::black_box(&prepared.samples),
        std::hint::black_box(&prepared.samples),
        std::hint::black_box(&mut prepared.output),
    );
    prepared
}

#[metabench::benchmark(VECTOR_ADD, "vector", "add", gungraun_setup = vector_prepare)]
#[bench::size_64(64_usize)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_512(512_usize)]
#[bench::size_1024(1024_usize)]
#[bench::size_4096(4096_usize)]
fn native_add(mut prepared: VectorPrepared) -> VectorPrepared {
    kithara_dsp::add(
        std::hint::black_box(&prepared.samples),
        std::hint::black_box(&mut prepared.output),
    );
    prepared
}

struct FadePrepared {
    curve: FadeCurve,
    positions: Vec<f32>,
    outgoing: Vec<f32>,
    incoming: Vec<f32>,
}

fn fade_prepare(index: usize) -> FadePrepared {
    let size = consts::SIZES[index % consts::SIZES.len()];
    let curve = [
        FadeCurve::EqualPower3dB,
        FadeCurve::EqualPower6dB,
        FadeCurve::SquareRoot,
        FadeCurve::Linear,
    ][index / consts::SIZES.len()];
    let mut positions = vec![0.0_f32; size];
    kithara_dsp::ramp(&mut positions, 0.0, 1.0);
    kithara_dsp::div(&mut positions, size.to_f32().expect("size is finite"));
    FadePrepared {
        curve,
        positions,
        outgoing: vec![0.0; size],
        incoming: vec![0.0; size],
    }
}

#[metabench::benchmark(FADE_GAINS, "fade_gains", "gains", gungraun_setup = fade_prepare)]
#[bench::equal_power_3db_size_64(0_usize)]
#[bench::equal_power_3db_size_128(1_usize)]
#[bench::equal_power_3db_size_256(2_usize)]
#[bench::equal_power_3db_size_512(3_usize)]
#[bench::equal_power_3db_size_1024(4_usize)]
#[bench::equal_power_3db_size_4096(5_usize)]
#[bench::equal_power_6db_size_64(6_usize)]
#[bench::equal_power_6db_size_128(7_usize)]
#[bench::equal_power_6db_size_256(8_usize)]
#[bench::equal_power_6db_size_512(9_usize)]
#[bench::equal_power_6db_size_1024(10_usize)]
#[bench::equal_power_6db_size_4096(11_usize)]
#[bench::square_root_size_64(12_usize)]
#[bench::square_root_size_128(13_usize)]
#[bench::square_root_size_256(14_usize)]
#[bench::square_root_size_512(15_usize)]
#[bench::square_root_size_1024(16_usize)]
#[bench::square_root_size_4096(17_usize)]
#[bench::linear_size_64(18_usize)]
#[bench::linear_size_128(19_usize)]
#[bench::linear_size_256(20_usize)]
#[bench::linear_size_512(21_usize)]
#[bench::linear_size_1024(22_usize)]
#[bench::linear_size_4096(23_usize)]
fn native_fade_gains(mut prepared: FadePrepared) -> FadePrepared {
    fade::gains(
        prepared.curve,
        std::hint::black_box(&prepared.positions),
        std::hint::black_box(&mut prepared.outgoing),
        std::hint::black_box(&mut prepared.incoming),
    );
    prepared
}

struct ProductPrepared {
    samples: Vec<f32>,
    gains: Vec<f32>,
}

fn product_prepare(size: usize) -> ProductPrepared {
    let samples = tone(size)
        .into_iter()
        .map(|sample| sample + 0.125)
        .collect();
    ProductPrepared {
        samples,
        gains: vec![0.7; size],
    }
}

#[metabench::benchmark(PRODUCT_SCALAR_SCALE, "render_products", "scalar_scale", gungraun_setup = product_prepare)]
#[bench::size_1(1_usize)]
#[bench::size_128(128_usize)]
#[bench::size_257(257_usize)]
#[bench::size_1023(1023_usize)]
fn native_scalar_scale(mut prepared: ProductPrepared) -> ProductPrepared {
    for sample in std::hint::black_box(&mut prepared.samples) {
        *sample *= 0.7;
    }
    prepared
}

#[metabench::benchmark(PRODUCT_SCALE, "render_products", "scale", gungraun_setup = product_prepare)]
#[bench::size_1(1_usize)]
#[bench::size_128(128_usize)]
#[bench::size_257(257_usize)]
#[bench::size_1023(1023_usize)]
fn native_scale(mut prepared: ProductPrepared) -> ProductPrepared {
    kithara_dsp::scale_in_place(std::hint::black_box(&mut prepared.samples), 0.7);
    prepared
}

#[metabench::benchmark(PRODUCT_SCALAR_PRODUCT, "render_products", "scalar_product", gungraun_setup = product_prepare)]
#[bench::size_1(1_usize)]
#[bench::size_128(128_usize)]
#[bench::size_257(257_usize)]
#[bench::size_1023(1023_usize)]
fn native_scalar_product(mut prepared: ProductPrepared) -> ProductPrepared {
    for (sample, gain) in std::hint::black_box(&mut prepared.samples)
        .iter_mut()
        .zip(std::hint::black_box(&prepared.gains))
    {
        *sample *= *gain;
    }
    prepared
}

#[metabench::benchmark(PRODUCT_PRODUCT, "render_products", "product", gungraun_setup = product_prepare)]
#[bench::size_1(1_usize)]
#[bench::size_128(128_usize)]
#[bench::size_257(257_usize)]
#[bench::size_1023(1023_usize)]
fn native_product(mut prepared: ProductPrepared) -> ProductPrepared {
    kithara_dsp::multiply_in_place(
        std::hint::black_box(&mut prepared.samples),
        std::hint::black_box(&prepared.gains),
    );
    prepared
}

struct DetectorPrepared {
    detector: kithara_dsp::filter::Fir<17, 7, 1, 9>,
    input: Vec<f32>,
    output: Vec<f32>,
}

fn detector_prepare(index: usize) -> DetectorPrepared {
    let frames = [128_usize, 256, 257, 600][index % 4];
    let amplitude = if index < 4 { 0.125 } else { 2.0 };
    let input = tone(frames)
        .into_iter()
        .map(|sample| sample * amplitude)
        .collect();
    DetectorPrepared {
        detector: kithara_dsp::filter::Fir::<17, 7, 1, 9>::new([[0.125; 17]; 7]),
        input,
        output: vec![0.0; frames],
    }
}

fn detector_action(mut prepared: DetectorPrepared) -> DetectorPrepared {
    prepared
        .detector
        .begin_block(&[prepared.input.as_mut_slice()], 0.98);
    prepared.detector.process(
        0,
        std::hint::black_box(prepared.input.as_slice()),
        std::hint::black_box(prepared.input.as_slice()),
        std::hint::black_box(prepared.output.as_mut_slice()),
    );
    std::hint::black_box(&prepared.output);
    prepared
}

#[metabench::benchmark(DETECTOR_QUIET, "fir_history", "quiet", gungraun_setup = detector_prepare)]
#[bench::size_128(0_usize)]
#[bench::size_256(1_usize)]
#[bench::size_257(2_usize)]
#[bench::size_600(3_usize)]
fn native_detector_quiet(prepared: DetectorPrepared) -> DetectorPrepared {
    detector_action(prepared)
}

#[metabench::benchmark(DETECTOR_OVERLOADED, "fir_history", "overloaded", gungraun_setup = detector_prepare)]
#[bench::size_128(4_usize)]
#[bench::size_256(5_usize)]
#[bench::size_257(6_usize)]
#[bench::size_600(7_usize)]
fn native_detector_overloaded(prepared: DetectorPrepared) -> DetectorPrepared {
    detector_action(prepared)
}

struct EnvelopePrepared {
    envelope: kithara_dsp::envelope::AttackRelease,
    targets: Vec<f32>,
}

fn envelope_prepare(frames: usize) -> EnvelopePrepared {
    EnvelopePrepared {
        envelope: kithara_dsp::envelope::AttackRelease::new(0.999_583_4),
        targets: [1.0, 0.25, 0.8, 0.99]
            .into_iter()
            .cycle()
            .take(frames)
            .collect(),
    }
}

#[metabench::benchmark(ENVELOPE_PROCESS, "attack_release", "process", gungraun_setup = envelope_prepare)]
#[bench::size_128(128_usize)]
#[bench::size_256(256_usize)]
#[bench::size_257(257_usize)]
#[bench::size_600(600_usize)]
fn native_envelope(mut prepared: EnvelopePrepared) -> EnvelopePrepared {
    prepared
        .envelope
        .process(std::hint::black_box(prepared.targets.as_mut_slice()));
    std::hint::black_box(prepared.targets.as_slice());
    prepared
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [
        VECTOR_RAMP,
        VECTOR_DIV,
        VECTOR_SCALE_IN_PLACE,
        VECTOR_AFFINE,
        VECTOR_SUB,
        VECTOR_SCALE_ADD,
        VECTOR_MUL_ADD,
        VECTOR_ADD,
        FADE_GAINS,
        PRODUCT_SCALAR_SCALE,
        PRODUCT_SCALE,
        PRODUCT_SCALAR_PRODUCT,
        PRODUCT_PRODUCT,
        DETECTOR_QUIET,
        DETECTOR_OVERLOADED,
        ENVELOPE_PROCESS,
    ],
    allocator = std::alloc::System,
);
