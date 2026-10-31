#![forbid(unsafe_code)]

use std::{
    f64::consts::{FRAC_1_SQRT_2, TAU},
    hint::black_box,
    num::NonZeroUsize,
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_dsp::{
    filter::{Biquad, Coefficients, Hertz, Type},
    interp::{Interpolation, interpolate},
    spectrum::{Autocorrelation, Fft, FftLen, magnitude, phase, prediction_difference},
};
use kithara_test_utils::{bufpool::pools, kithara_platform::time::Duration};
use num_traits::ToPrimitive;
use realfft::{RealToComplex, RealToComplexEven};

mod consts {
    use super::NonZeroUsize;

    pub(super) const SIZES: [usize; 6] = [64, 128, 256, 512, 1024, 4096];
    pub(super) const FFT_LENS: [usize; 3] = [1_024, 3_072, 4_096];
    pub(super) const TONE_STEP: f32 = 0.05;
    pub(super) const SIX: NonZeroUsize = NonZeroUsize::MIN.saturating_add(5);
    pub(super) const TWO: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);
}

fn kernels(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout");
    for frames in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("frame count fits u64"),
        ));
        let planes = vec![vec![0.25_f32; frames]; consts::SIX.get()];
        let planar = planes.concat();
        let stride = NonZeroUsize::new(frames).expect("bench sizes are non-zero");
        let mut restored = planes.clone();
        for channels in [consts::TWO, consts::SIX] {
            let mut interleaved = vec![0.0_f32; channels.get() * frames];
            group.bench_with_input(
                BenchmarkId::new(format!("fi_interleave_{channels}ch"), frames),
                &frames,
                |b, _| {
                    b.iter(|| {
                        fast_interleave::interleave_variable(
                            black_box(&planes[..channels.get()]),
                            0..frames,
                            &mut interleaved,
                            channels,
                        );
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("fi_deinterleave_{channels}ch"), frames),
                &frames,
                |b, _| {
                    b.iter(|| {
                        fast_interleave::deinterleave_variable(
                            black_box(&interleaved),
                            channels,
                            &mut restored,
                            0..frames,
                        );
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("interleave_{channels}ch"), frames),
                &frames,
                |b, _| {
                    b.iter(|| {
                        kithara_dsp::interleave_channel_major(
                            black_box(&planar[..channels.get() * frames]),
                            stride,
                            0..frames,
                            &mut interleaved,
                            channels,
                        );
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("deinterleave_{channels}ch"), frames),
                &frames,
                |b, _| {
                    b.iter(|| {
                        kithara_dsp::deinterleave_variable(
                            black_box(&interleaved),
                            channels,
                            &mut restored,
                            0..frames,
                        );
                    });
                },
            );
        }
        let mut noisy = vec![f32::from_bits(1); frames];
        group.bench_with_input(BenchmarkId::new("sanitize", frames), &frames, |b, _| {
            b.iter(|| kithara_dsp::sanitize(black_box(&mut noisy)));
        });
    }
    group.finish();
}

/// `len` samples of a sine advancing [`consts::TONE_STEP`] rad per sample.
fn tone(len: usize) -> Vec<f32> {
    std::iter::successors(Some(0.0_f32), |phase| Some(phase + consts::TONE_STEP))
        .map(f32::sin)
        .take(len)
        .collect()
}

/// The symmetric Hann window the analyzers built for themselves before
/// `spectrum::Fft`.
fn hann(len: usize) -> Vec<f32> {
    let span = len.saturating_sub(1).to_f64().unwrap_or(1.0);
    (0..len)
        .filter_map(|n| n.to_f64())
        .filter_map(|n| (0.5 - 0.5 * (TAU * n / span).cos()).to_f32())
        .collect()
}

fn vector(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector");
    for size in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(size).expect("size fits u64"),
        ));
        let samples = tone(size);
        group.bench_with_input(BenchmarkId::new("sum_squares", size), &size, |b, _| {
            b.iter(|| kithara_dsp::sum_squares(black_box(&samples)));
        });
    }
    group.finish();
}

fn downmix(c: &mut Criterion) {
    let mut group = c.benchmark_group("downmix");
    for frames in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("frame count fits u64"),
        ));
        let mut mono = vec![0.0_f32; frames];
        for channels in [consts::TWO, consts::SIX] {
            let interleaved = tone(channels.get() * frames);
            group.bench_with_input(
                BenchmarkId::new(format!("{channels}ch"), frames),
                &frames,
                |b, _| {
                    b.iter(|| kithara_dsp::downmix(black_box(&interleaved), channels, &mut mono));
                },
            );
        }
    }
    group.finish();
}

fn spectrum(c: &mut Criterion) {
    let region = pools();
    let mut group = c.benchmark_group("spectrum");
    for len in consts::FFT_LENS {
        group.throughput(Throughput::Elements(
            u64::try_from(len).expect("length fits u64"),
        ));
        let frame = tone(len);
        let fft =
            Fft::new(FftLen::new(len).expect("bench lengths are f·2ⁿ")).expect("the FFT builds");
        let mut bins = fft.spectrum(&region).expect("the planes fit the region");
        group.bench_with_input(BenchmarkId::new("fft", len), &len, |b, _| {
            b.iter(|| fft.forward(black_box(&frame), &mut bins).is_ok());
        });
        let window = hann(len);
        let reference: RealToComplexEven<f32> =
            RealToComplexEven::new(len, &mut rustfft::FftPlanner::new());
        let mut input = reference.make_input_vec();
        let mut output = reference.make_output_vec();
        let mut scratch = reference.make_scratch_vec();
        group.bench_with_input(BenchmarkId::new("realfft", len), &len, |b, _| {
            b.iter(|| {
                for ((slot, sample), weight) in input.iter_mut().zip(black_box(&frame)).zip(&window)
                {
                    *slot = sample * weight;
                }
                reference
                    .process_with_scratch(&mut input, &mut output, &mut scratch)
                    .is_ok()
            });
        });
    }
    group.finish();
}

fn bins(c: &mut Criterion) {
    let mut group = c.benchmark_group("bins");
    for size in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(size).expect("size fits u64"),
        ));
        let re = tone(size);
        let im: Vec<f32> = re.iter().rev().copied().collect();
        let mut output = vec![0.0_f32; size];
        group.bench_with_input(BenchmarkId::new("magnitude", size), &size, |b, _| {
            b.iter(|| magnitude(black_box(&re), black_box(&im), &mut output));
        });
        group.bench_with_input(BenchmarkId::new("phase", size), &size, |b, _| {
            b.iter(|| phase(black_box(&re), black_box(&im), &mut output));
        });
    }
    group.finish();
}

fn prediction(c: &mut Criterion) {
    let mut group = c.benchmark_group("prediction_difference");
    group.sample_size(20);
    for size in [1, 3, 17, 64, 513, 1024] {
        let re = tone(size);
        let im: Vec<f32> = re.iter().rev().copied().collect();
        let mut history: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; size + 1]);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                prediction_difference(
                    [black_box(&re), black_box(&im)],
                    black_box(&mut history).each_mut().map(Vec::as_mut_slice),
                )
            });
        });
    }
    group.finish();
}

fn scalar_phase(re: &[f32], im: &[f32], output: &mut [f32]) -> usize {
    let len = re.len().min(im.len()).min(output.len());
    for ((x, y), slot) in re.iter().zip(im).zip(output) {
        *slot = y.atan2(*x);
    }
    len
}

fn phase_shapes(c: &mut Criterion) {
    type Kernel = fn(&[f32], &[f32], &mut [f32]) -> usize;
    let mut cases: Vec<_> = [0, 1, 2, 7, 255, 256, 257, 512, 513, 1025]
        .into_iter()
        .map(|len| (format!("exact-{len}"), [len; 3], [0; 3]))
        .collect();
    for len in [7, 513, 1025] {
        for (name, offsets) in [("re", [1, 0, 0]), ("im", [0, 2, 0]), ("out", [0, 0, 3])] {
            cases.push((format!("offset-{name}-{len}"), [len; 3], offsets));
        }
    }
    for (name, lengths) in [
        ("re", [513, 515, 516]),
        ("im", [515, 513, 516]),
        ("out", [515, 516, 513]),
    ] {
        cases.push((format!("prefix-{name}-513"), lengths, [1, 2, 3]));
    }
    let mut group = c.benchmark_group("dsp_phase");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(1));
    for (name, [nr, ni, no], [r, i, o]) in cases {
        let mut re = vec![0.0; r];
        re.extend(tone(nr));
        let mut im = vec![0.0; i];
        im.extend(tone(ni).into_iter().rev());
        let mut output = vec![f32::MIN; no + o + 1];
        let len = nr.min(ni).min(no);
        group.throughput(Throughput::Elements(
            u64::try_from(len).expect("bin count fits u64"),
        ));
        for (operation, kernel) in [
            ("phase", phase as Kernel),
            ("scalar_atan2", scalar_phase as Kernel),
            ("magnitude_control", magnitude as Kernel),
        ] {
            output.fill(f32::MIN);
            assert_eq!(kernel(&re[r..], &im[i..], &mut output[o..o + no]), len);
            for ((real, imaginary), got) in re[r..].iter().zip(&im[i..]).zip(&output[o..o + len]) {
                let (want, tolerance) = if operation == "magnitude_control" {
                    let want = real.hypot(*imaginary);
                    (want, 2.0 * f32::EPSILON * want)
                } else {
                    (imaginary.atan2(*real), 1e-6)
                };
                assert!(
                    (got - want).abs() <= tolerance,
                    "{operation}/{name}: {got} differs from scalar {want}"
                );
            }
            assert!(
                output[..o]
                    .iter()
                    .chain(&output[o + len..])
                    .all(|x| *x == f32::MIN)
            );
            group.bench_function(BenchmarkId::new(operation, &name), |b| {
                b.iter(|| {
                    black_box(kernel(
                        black_box(&re[r..]),
                        black_box(&im[i..]),
                        black_box(&mut output[o..o + no]),
                    ));
                    black_box(&output);
                });
            });
        }
    }
    group.finish();
}

fn autocorrelation(c: &mut Criterion) {
    let region = pools();
    let mut group = c.benchmark_group("autocorrelation");
    for size in consts::SIZES {
        group.throughput(Throughput::Elements(
            u64::try_from(size).expect("size fits u64"),
        ));
        let frame = tone(size);
        let mut acf = Autocorrelation::new(
            NonZeroUsize::new(size).expect("bench sizes are non-zero"),
            &region,
        )
        .expect("the padding fits the region");
        let mut output = vec![0.0_f32; size];
        group.bench_with_input(BenchmarkId::new("process", size), &size, |b, _| {
            b.iter(|| acf.process(black_box(&frame), &mut output));
        });
    }
    group.finish();
}

fn low_pass() -> Coefficients<f64> {
    Coefficients::from_params(
        Type::LowPass,
        Hertz::from_hz(48_000.0).expect("positive rate"),
        Hertz::from_hz(4_000.0).expect("positive cutoff"),
        FRAC_1_SQRT_2,
    )
    .expect("valid low-pass")
}

fn biquad(c: &mut Criterion) {
    const FRAMES: usize = 1_024;
    let low_pass = low_pass();
    let mut group = c.benchmark_group("biquad");
    group.throughput(Throughput::Elements(
        u64::try_from(FRAMES).expect("frame count fits u64"),
    ));
    for channels in [1_usize, 2, 8] {
        for sections in [1_usize, 4] {
            let mut filter = Biquad::new(
                NonZeroUsize::new(channels).expect("bench channels are non-zero"),
                NonZeroUsize::new(sections).expect("bench sections are non-zero"),
            )
            .expect("filter builds");
            for section in 0..sections {
                filter.retune(section, low_pass).expect("section in range");
            }
            let mut planes = vec![vec![0.25_f32; FRAMES]; channels];
            group.bench_with_input(
                BenchmarkId::new(format!("{channels}ch"), sections),
                &sections,
                |b, _| {
                    b.iter(|| filter.process(black_box(&mut planes), 0..FRAMES).is_ok());
                },
            );
        }
    }
    group.finish();
}

fn biquad_decay(c: &mut Criterion) {
    const FRAMES: usize = 1_024;
    let low_pass = low_pass();
    let mut group = c.benchmark_group("biquad_decay");
    group.throughput(Throughput::Elements(
        u64::try_from(FRAMES).expect("frame count fits u64"),
    ));
    let mut filter = Biquad::new(consts::TWO, NonZeroUsize::MIN).expect("filter builds");
    filter.retune(0, low_pass).expect("section 0 exists");
    let burst: Vec<f32> = (0..FRAMES)
        .map(|frame| if frame < 64 { 0.5 } else { 0.0 })
        .collect();
    let template = vec![burst; consts::TWO.get()];
    let mut planes = template.clone();
    group.bench_function("burst_into_silence_2ch", |b| {
        b.iter(|| {
            planes.clone_from(&template);
            for start in (0..FRAMES).step_by(64) {
                let _ = black_box(filter.process(&mut planes, start..start + 64));
            }
        });
    });
    group.finish();
}

fn interp(c: &mut Criterion) {
    const FRAMES: usize = 1_024;
    let window: Vec<f32> = std::iter::successors(Some(0.0_f32), |phase| Some(phase + 0.05))
        .map(f32::sin)
        .take(FRAMES + 4)
        .collect();
    let mut group = c.benchmark_group("interp");
    for ratio in [0.5_f64, 1.001, 2.0] {
        let positions: Vec<f32> =
            std::iter::successors(Some(1.0_f64), |position| Some(position + ratio))
                .take_while(|position| *position < 1_024.0)
                .filter_map(|position| position.to_f32())
                .collect();
        let mut output = vec![0.0_f32; positions.len()];
        group.throughput(Throughput::Elements(
            u64::try_from(positions.len()).expect("position count fits u64"),
        ));
        for method in [
            Interpolation::Linear,
            Interpolation::Quadratic,
            Interpolation::Hermite,
            Interpolation::Watte,
        ] {
            group.bench_with_input(
                BenchmarkId::new(format!("{method:?}"), ratio),
                &ratio,
                |b, _| {
                    b.iter(|| {
                        interpolate(
                            method,
                            black_box(&window),
                            black_box(&positions),
                            &mut output,
                        )
                        .is_ok()
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    kernels,
    vector,
    downmix,
    biquad,
    biquad_decay,
    interp,
    spectrum,
    bins,
    phase_shapes,
    prediction,
    autocorrelation
);
criterion_main!(benches);
