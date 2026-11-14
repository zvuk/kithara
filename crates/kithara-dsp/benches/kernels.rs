#![forbid(unsafe_code)]

use std::{
    f64::consts::{FRAC_1_SQRT_2, TAU},
    hint::black_box,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_dsp::{
    curve::CurveShape,
    filter::{Biquad, Coefficients, Hertz, Response, StateVariableFilter, Type},
    interp::{Interpolation, interpolate, interpolate_frame, weighted_frames},
    spectrum::{Autocorrelation, Fft, FftLen, prediction_difference},
};
use kithara_test_utils::bufpool::pools;
use num_traits::{AsPrimitive, ToPrimitive};
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

fn curve(c: &mut Criterion) {
    for (name, shape) in [
        ("hold", CurveShape::HOLD),
        ("linear", CurveShape::LINEAR),
        (
            "cubic",
            CurveShape::cubic(0.2, 0.6).expect("ordered handles"),
        ),
    ] {
        let mut group = c.benchmark_group(format!("curve_{name}"));
        for frames in [128_u32, 512] {
            let positions: Vec<_> = (0..frames)
                .map(|frame| f64::from(frame) / f64::from(frames - 1))
                .collect();
            for native in [true, false] {
                group.bench_with_input(
                    BenchmarkId::new(if native { "native" } else { "scalar" }, frames),
                    &positions,
                    |b, positions| {
                        b.iter_batched_ref(
                            || positions.clone(),
                            |values| {
                                if native {
                                    shape.process(black_box(values), black_box([0.75, 0.25]));
                                } else {
                                    scalar_curve(name, black_box(values), black_box([0.75, 0.25]));
                                }
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
            }
        }
        group.finish();
    }
}

fn scalar_curve(shape: &str, values: &mut [f64], [left, right]: [f64; 2]) {
    let write = |time: f64, progress: f64| {
        if time >= 1.0 {
            right
        } else if time <= 0.0 || left == right || progress == 0.0 {
            left
        } else if progress == 1.0 {
            right
        } else {
            left + (right - left) * progress
        }
    };
    match shape {
        "hold" => {
            for value in values {
                *value = write(*value, 0.0);
            }
        }
        "linear" => {
            for value in values {
                *value = write(*value, *value);
            }
        }
        "cubic" => {
            for value in values {
                let time = *value;
                let remaining = 1.0 - time;
                let a = 3.0 * remaining * remaining * time * 0.2;
                let b = 3.0 * remaining * time * time * 0.6;
                *value = write(time, ((a + b) + time * time * time).clamp(0.0, 1.0));
            }
        }
        _ => unreachable!("registered curve"),
    }
}

fn ratio_ramp(c: &mut Criterion) {
    let mut group = c.benchmark_group("ratio_ramp");
    for frames in [128, 512] {
        let mut output = vec![0.0; frames];
        for native in [true, false] {
            group.bench_function(
                BenchmarkId::new(if native { "native" } else { "scalar" }, frames),
                |b| {
                    b.iter(|| {
                        let output = black_box(&mut output);
                        let first = black_box((1_u64 << 25) + 3);
                        let divisor = black_box(NonZeroU64::new(997).expect("positive"));
                        if native {
                            kithara_dsp::ratio_ramp(output, first, divisor);
                        } else {
                            let divisor: f64 = divisor.get().as_();
                            for (index, value) in output.iter_mut().enumerate() {
                                let numerator: f64 = first.saturating_add(index.as_()).as_();
                                *value = numerator / divisor;
                            }
                        }
                    });
                },
            );
        }
    }
    group.finish();
}

fn narrow_f64(c: &mut Criterion) {
    let mut group = c.benchmark_group("narrow_f64");
    for frames in [128, 512] {
        let input: Vec<_> = tone(frames).into_iter().map(f64::from).collect();
        let mut output = vec![0.0; frames];
        for native in [true, false] {
            group.bench_function(
                BenchmarkId::new(if native { "native" } else { "scalar" }, frames),
                |b| {
                    b.iter(|| {
                        let (input, output) = (black_box(&input), black_box(&mut output));
                        if native {
                            kithara_dsp::narrow_f64(input, output);
                        } else {
                            for (value, input) in output.iter_mut().zip(input) {
                                *value = input.as_();
                            }
                        }
                    });
                },
            );
        }
    }
    group.finish();
}

fn multiply_squared(c: &mut Criterion) {
    let mut group = c.benchmark_group("multiply_squared_in_place");
    for frames in [128, 512] {
        let input = tone(frames);
        let square = vec![0.7; frames];
        let gains = vec![0.3; frames];
        for native in [true, false] {
            group.bench_function(
                BenchmarkId::new(if native { "native" } else { "scalar" }, frames),
                |b| {
                    b.iter_batched_ref(
                        || input.clone(),
                        |output| {
                            let (output, square, gains) =
                                (black_box(output), black_box(&square), black_box(&gains));
                            if native {
                                kithara_dsp::multiply_squared_in_place(output, square, gains);
                            } else {
                                for ((value, square), gain) in
                                    output.iter_mut().zip(square).zip(gains)
                                {
                                    *value = (*value * (square * square)) * gain;
                                }
                            }
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
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

fn svf(c: &mut Criterion) {
    let mut group = c.benchmark_group("svf");
    let rate = NonZeroU32::new(48_000).expect("bench rate");
    for frames in [128_usize, 1_024] {
        let tone: Vec<f32> =
            std::iter::successors(Some(0.0_f32), |phase| Some(phase + consts::TONE_STEP))
                .take(frames)
                .map(|phase| 0.25 * phase.sin())
                .collect();
        let template = [tone.clone(), tone];
        let cutoffs = vec![4_000.0; frames];
        group.throughput(Throughput::Elements(
            u64::try_from(frames * 2).expect("stereo frame count fits u64"),
        ));
        for response in [Response::LowPass, Response::HighPass] {
            let mut filter = StateVariableFilter::<2>::new();
            let mut probe = template.clone();
            assert!(
                filter
                    .process(
                        &mut probe,
                        0..frames,
                        &cutoffs,
                        rate,
                        FRAC_1_SQRT_2,
                        response
                    )
                    .is_ok()
            );
            assert!(probe.iter().flatten().all(|sample| sample.is_finite()));
            assert!(probe.iter().flatten().any(|sample| *sample != 0.0));
            filter.reset();
            group.bench_with_input(
                BenchmarkId::new(format!("2ch_{response:?}"), frames),
                &frames,
                |b, &frames| {
                    b.iter_batched_ref(
                        || template.clone(),
                        |planes| {
                            let result = filter.process(
                                black_box(&mut *planes),
                                0..frames,
                                black_box(&cutoffs),
                                rate,
                                FRAC_1_SQRT_2,
                                response,
                            );
                            black_box(result.is_ok());
                            black_box(planes);
                        },
                        BatchSize::SmallInput,
                    );
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

type Crossfade = fn(&[f32], &mut [f32], NonZeroUsize, usize, usize) -> usize;

// The scalar audio-join law is an independent timing control.
fn previous_audio_crossfade(
    outgoing: &[f32],
    output: &mut [f32],
    channels: NonZeroUsize,
    first: usize,
    frames: usize,
) -> usize {
    let mut frame = u16::try_from(first).expect("benchmark position");
    let frames = u16::try_from(frames).expect("benchmark denominator");
    for (old, incoming) in outgoing
        .chunks_exact(channels.get())
        .zip(output.chunks_exact_mut(channels.get()))
    {
        if frame >= frames {
            break;
        }
        let gain = f32::from(frame) / f32::from(frames);
        for (&old, incoming) in old.iter().zip(incoming) {
            if old.to_bits() != incoming.to_bits() {
                *incoming = old.mul_add(1.0 - gain, *incoming * gain);
            }
        }
        frame += 1;
    }
    usize::from(frame) - first
}

fn crossfade(c: &mut Criterion) {
    let old_sample = 0.314_159_f32;
    let new_sample = -0.271_828_f32;
    let kernels: [(&str, Crossfade); 2] = [
        ("crossfade_linear", kithara_dsp::crossfade_linear),
        ("crossfade_previous_audio", previous_audio_crossfade),
    ];
    for (name, kernel) in kernels {
        let mut group = c.benchmark_group(name);
        group.sample_size(20);
        let mut shapes = Vec::new();
        for frames in consts::SIZES {
            for channels in [NonZeroUsize::MIN, consts::TWO, consts::SIX] {
                shapes.push((channels, frames, frames.to_string(), 0, frames, new_sample));
            }
        }
        shapes.extend([
            (consts::TWO, 7, "7".to_owned(), 0, 7, new_sample),
            (consts::SIX, 127, "127".to_owned(), 0, 127, new_sample),
            (consts::TWO, 127, "127-tail".to_owned(), 1, 127, new_sample),
            (
                consts::TWO,
                4096,
                "4096-chunk128".to_owned(),
                0,
                128,
                new_sample,
            ),
            (consts::TWO, 7, "7-identity".to_owned(), 0, 7, old_sample),
        ]);
        for (channels, frames, shape, tail, chunk, input_sample) in shapes {
            let old = vec![old_sample; frames * channels.get() + tail];
            let apply = |output: &mut [f32]| {
                let mut processed = 0;
                for (outgoing, incoming) in black_box(&old)
                    .chunks(chunk * channels.get())
                    .zip(output.chunks_mut(chunk * channels.get()))
                {
                    processed += kernel(outgoing, incoming, channels, processed, frames);
                }
                processed
            };
            let mut expected = vec![input_sample; old.len()];
            assert_eq!(
                previous_audio_crossfade(&old, &mut expected, channels, 0, frames),
                frames
            );
            let mut actual = vec![input_sample; old.len()];
            assert_eq!(apply(&mut actual), frames);
            assert!(
                actual
                    .iter()
                    .zip(&expected)
                    .all(|(actual, expected)| actual.to_bits() == expected.to_bits()),
                "kernel timing control must preserve the scalar law and tail"
            );
            group.throughput(Throughput::Elements(
                u64::try_from(frames).expect("frame count"),
            ));
            group.bench_function(BenchmarkId::new(format!("{channels}ch"), shape), |b| {
                b.iter_batched_ref(
                    || vec![input_sample; old.len()],
                    |output| black_box(apply(black_box(output))),
                    BatchSize::SmallInput,
                );
            });
        }
        group.finish();
    }
}

fn source_frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("source_frames");
    for channels in [1_usize, 2, 4, 6, 9] {
        let samples = vec![0.25; channels * 33];
        let offsets = std::array::from_fn::<_, 33, _>(|tap| tap * channels);
        let weights = [0.125; 33];
        let mut frame = vec![0.0; channels];
        let mut sums = vec![0.0; channels];
        group.throughput(Throughput::Elements(
            u64::try_from(channels).expect("channels"),
        ));
        group.bench_function(BenchmarkId::new("interpolate", channels), |b| {
            b.iter(|| {
                interpolate_frame(
                    black_box(&samples[..channels]),
                    black_box(&samples[channels..2 * channels]),
                    black_box(0.531_589_7),
                    &mut frame,
                );
                black_box(&frame);
            });
        });
        group.bench_function(BenchmarkId::new("weighted", channels), |b| {
            b.iter(|| {
                weighted_frames(
                    black_box(&samples),
                    black_box(&offsets),
                    black_box(&weights),
                    channels,
                    &mut sums,
                )
                .expect("resolved taps");
                black_box(&sums);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    crossfade,
    kernels,
    vector,
    curve,
    ratio_ramp,
    narrow_f64,
    multiply_squared,
    downmix,
    biquad,
    biquad_decay,
    svf,
    interp,
    source_frames,
    spectrum,
    prediction,
    autocorrelation
);
criterion_main!(benches);
