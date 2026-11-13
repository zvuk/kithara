use std::{
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    ops::Deref,
};

use biquad::{Biquad as _, DirectForm1};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara_dsp::{
    filter::{Biquad, Coefficients, Crossover, Hertz, Type},
    param::{DEFAULT_GAIN_SPAN, MIN_SETTLE_RATIO, SmoothingFilter, SmoothingFilterCoeff},
};
use kithara_test_fixtures::portable;
use num_traits::AsPrimitive;

const RATE: f32 = 48_000.0;
const CHANNELS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

#[derive(Clone, Copy, Debug)]
enum Design {
    WidenedF32,
    NativeF64,
}

fn frequencies(bands: usize) -> Vec<f32> {
    let last: f32 = (bands - 1).as_();
    let first = 60.0_f32.log10();
    let step = (18_000.0_f32.log10() - first) / last;
    let centers: Vec<_> = (0..bands)
        .map(|band| {
            let band: f32 = band.as_();
            10.0_f32.powf(band.mul_add(step, first))
        })
        .collect();
    centers
        .windows(2)
        .map(|pair| (pair[0] * pair[1]).sqrt())
        .collect()
}

fn coefficients(kind: Type<f32>, frequency: f32, design: Design) -> Coefficients<f64> {
    match design {
        Design::WidenedF32 => {
            let Coefficients { a1, a2, b0, b1, b2 } = Coefficients::from_normalized_params(
                kind,
                2.0 * frequency / RATE,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .expect("valid f32 benchmark design");
            Coefficients {
                a1: f64::from(a1),
                a2: f64::from(a2),
                b0: f64::from(b0),
                b1: f64::from(b1),
                b2: f64::from(b2),
            }
        }
        Design::NativeF64 => {
            let kind = match kind {
                Type::LowPass => Type::LowPass,
                Type::HighPass => Type::HighPass,
                Type::AllPass => Type::AllPass,
                _ => unreachable!("benchmark uses only LP, HP and allpass"),
            };
            Coefficients::from_normalized_params(
                kind,
                2.0 * f64::from(frequency) / f64::from(RATE),
                std::f64::consts::FRAC_1_SQRT_2,
            )
            .expect("valid f64 benchmark design")
        }
    }
}

struct LegacySection(DirectForm1<f32>);

impl LegacySection {
    fn new(kind: Type<f32>, frequency: f32) -> Self {
        Self(DirectForm1::new(
            Coefficients::from_normalized_params(
                kind,
                2.0 * frequency / RATE,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .expect("legacy f32 design"),
        ))
    }

    fn run(&mut self, input: f32) -> f32 {
        let output = self.0.run(input);
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.0.reset_state();
            return 0.0;
        }
        output
    }
}

struct LegacySplit {
    low: [LegacySection; 2],
    high: [LegacySection; 2],
    allpass: Vec<LegacySection>,
}

const LEGACY_GAIN_FRAMES: usize = 128;

struct Legacy {
    splits: Vec<LegacySplit>,
    frame: Vec<f32>,
    gain_tiles: Vec<[f32; LEGACY_GAIN_FRAMES]>,
}

impl Legacy {
    fn new(frequencies: &[f32]) -> Self {
        Self {
            splits: frequencies
                .iter()
                .enumerate()
                .map(|(index, frequency)| LegacySplit {
                    low: std::array::from_fn(|_| LegacySection::new(Type::LowPass, *frequency)),
                    high: std::array::from_fn(|_| LegacySection::new(Type::HighPass, *frequency)),
                    allpass: frequencies[index + 1..]
                        .iter()
                        .map(|frequency| LegacySection::new(Type::AllPass, *frequency))
                        .collect(),
                })
                .collect(),
            frame: vec![0.0; frequencies.len() + 1],
            gain_tiles: vec![[0.0; LEGACY_GAIN_FRAMES]; frequencies.len() + 1],
        }
    }

    fn sum<P: Deref<Target = [f32]>>(&mut self, samples: &mut [f32], gains: &[P]) {
        let count = samples.len();
        for (chunk, start) in samples
            .chunks_mut(LEGACY_GAIN_FRAMES)
            .zip((0..count).step_by(LEGACY_GAIN_FRAMES))
        {
            for (tile, gain) in self.gain_tiles.iter_mut().zip(gains) {
                tile[..chunk.len()].copy_from_slice(&gain[start..start + chunk.len()]);
            }
            self.weighted_block(chunk);
        }
    }

    #[inline(never)]
    fn weighted_block(&mut self, samples: &mut [f32]) {
        for (sample, frame) in samples.iter_mut().zip(0..LEGACY_GAIN_FRAMES) {
            self.filter_sample(*sample);
            *sample = self
                .frame
                .iter()
                .zip(&self.gain_tiles)
                .fold(0.0, |output, (value, gain)| {
                    value.mul_add(gain[frame], output)
                });
        }
    }

    #[inline(always)]
    fn filter_sample(&mut self, mut high: f32) {
        let (high_band, lower) = self.frame.split_last_mut().expect("legacy high band");
        for (split, low) in self.splits.iter_mut().zip(lower) {
            *low = split.low[0].run(high);
            *low = split.low[1].run(*low);
            high = split.high[0].run(high);
            high = split.high[1].run(high);
            for section in &mut split.allpass {
                *low = section.run(*low);
            }
        }
        *high_band = high;
    }
}

struct Frame([Legacy; 2]);

impl Frame {
    fn new(frequencies: &[f32]) -> Self {
        Self(std::array::from_fn(|_| Legacy::new(frequencies)))
    }

    fn sum<P: Deref<Target = [f32]>>(
        &mut self,
        io: &mut [Vec<f32>; 2],
        gains: &[P],
        frames: usize,
    ) {
        for (filter, plane) in self.0.iter_mut().zip(io) {
            filter.sum(&mut plane[..frames], gains);
        }
    }
}

struct Svf {
    filter: Crossover<2>,
    bands: Vec<[Vec<f32>; 2]>,
}

impl Svf {
    fn new(frequencies: &[f32], frames: usize) -> Self {
        let cutoffs: Vec<_> = frequencies
            .iter()
            .map(|frequency| Hertz::from_hz(f64::from(*frequency)).expect("positive cutoff"))
            .collect();
        Self {
            filter: Crossover::new(
                &cutoffs,
                Hertz::from_hz(f64::from(RATE)).expect("positive rate"),
            )
            .expect("TPT design"),
            bands: (0..frequencies.len() + 1)
                .map(|_| std::array::from_fn(|_| vec![0.0; frames]))
                .collect(),
        }
    }

    fn sum<P: Deref<Target = [f32]>>(
        &mut self,
        io: &mut [Vec<f32>; 2],
        gains: &[P],
        frames: usize,
    ) {
        let input = io.each_ref().map(|plane| &plane[..frames]);
        assert_eq!(self.filter.split(&input, &mut self.bands), frames);
        for plane in &mut *io {
            plane[..frames].fill(0.0);
        }
        for (band, gain) in self.bands.iter().zip(gains) {
            for (plane, output) in band.iter().zip(&mut *io) {
                kithara_dsp::mul_add(&plane[..frames], &gain[..frames], &mut output[..frames]);
            }
        }
    }
}

struct Block {
    low: Vec<Biquad>,
    high: Vec<Biquad>,
    bands: Vec<[Vec<f32>; 2]>,
}

impl Block {
    fn new(frequencies: &[f32], frames: usize, design: Design) -> Self {
        let mut low = Vec::new();
        let mut high = Vec::new();
        for (index, &frequency) in frequencies.iter().enumerate() {
            let later = &frequencies[index + 1..];
            let mut lp = Biquad::new(
                CHANNELS,
                NonZeroUsize::new(2 + later.len()).expect("two low-pass sections"),
            )
            .expect("low-pass storage");
            let mut hp = Biquad::new(CHANNELS, NonZeroUsize::new(2).expect("LR4 sections"))
                .expect("high-pass storage");
            for section in 0..2 {
                lp.retune(section, coefficients(Type::LowPass, frequency, design))
                    .expect("low-pass coefficients");
                hp.retune(section, coefficients(Type::HighPass, frequency, design))
                    .expect("high-pass coefficients");
            }
            for (section, &frequency) in later.iter().enumerate() {
                lp.retune(section + 2, coefficients(Type::AllPass, frequency, design))
                    .expect("allpass compensation");
            }
            low.push(lp);
            high.push(hp);
        }
        Self {
            low,
            high,
            bands: frequencies
                .iter()
                .map(|_| std::array::from_fn(|_| vec![0.0; frames]))
                .collect(),
        }
    }

    fn sum<P: Deref<Target = [f32]>>(
        &mut self,
        io: &mut [Vec<f32>; 2],
        gains: &[P],
        frames: usize,
    ) {
        for ((low, high), band) in self.low.iter_mut().zip(&mut self.high).zip(&mut self.bands) {
            for (band, input) in band.iter_mut().zip(io.iter()) {
                band[..frames].copy_from_slice(&input[..frames]);
            }
            low.process(band, 0..frames)
                .expect("prepared low-pass planes");
            high.process(io, 0..frames)
                .expect("prepared high-pass planes");
        }
        let high_gain = gains.last().expect("one high band");
        for (channel, plane) in io.iter_mut().enumerate() {
            for (frame, sample) in plane.iter_mut().take(frames).enumerate() {
                let lower = self.bands.iter().zip(gains).fold(0.0, |sum, (band, gain)| {
                    band[channel][frame].mul_add(gain[frame], sum)
                });
                *sample = sample.mul_add(high_gain[frame], lower);
            }
        }
    }
}

fn gains(bands: usize, frames: usize, moving: bool) -> Vec<Vec<f32>> {
    let coeff = SmoothingFilterCoeff::new(
        NonZeroU32::new(48_000).expect("benchmark rate"),
        0.01,
        MIN_SETTLE_RATIO,
    );
    let targets = [10.0_f32.powf(6.0 / 20.0), 0.0, 1.0];
    (0..bands)
        .map(|band| {
            let mut smoother = SmoothingFilter::new(1.0);
            (0..frames)
                .map(|frame| {
                    let target = if moving {
                        targets[(frame / 128 + band) % targets.len()]
                    } else {
                        1.0
                    };
                    smoother.process(target, coeff);
                    smoother.try_settle(target, DEFAULT_GAIN_SPAN, MIN_SETTLE_RATIO);
                    smoother.z1
                })
                .collect()
        })
        .collect()
}

#[derive(Default)]
struct Difference {
    finite_pairs: usize,
    max_abs: f64,
    bit_differences: usize,
    nonfinite: [usize; 2],
    subnormal: [usize; 2],
    last_nonzero_frame: [Option<usize>; 2],
}

impl Difference {
    fn record(&mut self, frame: usize, expected: f32, actual: f32) {
        if expected.is_finite() && actual.is_finite() {
            self.finite_pairs += 1;
            self.max_abs = self
                .max_abs
                .max((f64::from(expected) - f64::from(actual)).abs());
        }
        self.bit_differences += usize::from(expected.to_bits() != actual.to_bits());
        for (index, value) in [expected, actual].into_iter().enumerate() {
            self.nonfinite[index] += usize::from(!value.is_finite());
            self.subnormal[index] += usize::from(value.is_subnormal());
            if value != 0.0 {
                self.last_nonzero_frame[index] =
                    Some(self.last_nonzero_frame[index].map_or(frame, |last| last.max(frame)));
            }
        }
    }
}

fn verify_difference_accounting() {
    let mut difference = Difference::default();
    let tiny = f32::from_bits(1);
    difference.record(7, tiny, 0.0);
    difference.record(3, 0.0, tiny);
    difference.record(9, f32::NAN, f32::INFINITY);
    difference.record(2, tiny, 0.0);
    assert_eq!(difference.finite_pairs, 3);
    assert_eq!(difference.max_abs, f64::from(tiny));
    assert_eq!(difference.nonfinite, [1, 1]);
    assert_eq!(difference.subnormal, [2, 1]);
    assert_eq!(difference.last_nonzero_frame, [Some(9), Some(9)]);
    assert_eq!(difference.bit_differences, 4);
}

trait Diagnostic {
    fn sum(&mut self, io: &mut [Vec<f32>; 2], gains: &[&[f32]], frames: usize);
}

impl Diagnostic for Svf {
    fn sum(&mut self, io: &mut [Vec<f32>; 2], gains: &[&[f32]], frames: usize) {
        self.sum(io, gains, frames);
    }
}

impl Diagnostic for Block {
    fn sum(&mut self, io: &mut [Vec<f32>; 2], gains: &[&[f32]], frames: usize) {
        self.sum(io, gains, frames);
    }
}

fn diagnose<K: Diagnostic>(frequencies: &[f32], frames: usize, label: &str, make: impl Fn() -> K) {
    for (case, left) in [
        ("sine_440", portable::eq_sine_440().to_vec()),
        (
            "finite",
            (0_u16..1024)
                .map(|index| (f32::from(index) * 0.1).sin())
                .collect(),
        ),
        ("impulse_tail", {
            let mut samples = vec![0.0; 192_001];
            samples[0] = 1.0;
            samples
        }),
        ("silence", vec![0.0; 8820]),
        (
            "subnormal_input",
            [f32::MIN_POSITIVE, f32::from_bits(1), -f32::from_bits(1)]
                .into_iter()
                .cycle()
                .take(8192)
                .collect(),
        ),
        (
            "retirement_bits",
            [
                0.25,
                -0.5,
                1.0,
                f32::MAX,
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                -0.0,
                f32::MIN_POSITIVE,
                f32::from_bits(1),
                -f32::from_bits(1),
            ]
            .into_iter()
            .chain(std::iter::repeat_n(0.0, 8192))
            .collect(),
        ),
    ] {
        let input = [
            left.clone(),
            left.iter().map(|sample| -0.75 * sample).collect(),
        ];
        for moving in [false, true] {
            let gains = gains(frequencies.len() + 1, left.len(), moving);
            let mut baseline = Frame::new(frequencies);
            let mut candidate = make();
            let mut expected = std::array::from_fn(|_| vec![0.0; frames]);
            let mut actual = expected.clone();
            let mut difference = Difference::default();
            for start in (0..left.len()).step_by(frames) {
                let end = (start + frames).min(left.len());
                let count = end - start;
                for ((expected, actual), input) in expected.iter_mut().zip(&mut actual).zip(&input)
                {
                    expected[..count].copy_from_slice(&input[start..end]);
                    actual[..count].copy_from_slice(&input[start..end]);
                }
                let gains: Vec<_> = gains.iter().map(|gain| &gain[start..end]).collect();
                baseline.sum(&mut expected, &gains, count);
                candidate.sum(&mut actual, &gains, count);
                for (expected, actual) in expected.iter().zip(&actual) {
                    for (offset, (&expected, &actual)) in
                        expected.iter().zip(actual).take(count).enumerate()
                    {
                        difference.record(start + offset, expected, actual);
                    }
                }
            }
            println!(
                "crossover_delta bands={} block={frames} candidate={label} case={case} moving={moving} finite_pairs={} max_abs_finite={:e} bit_differences={} nonfinite_df1={} nonfinite_candidate={} subnormal_df1={} subnormal_candidate={} last_nonzero_df1={:?} last_nonzero_candidate={:?}",
                frequencies.len() + 1,
                difference.finite_pairs,
                difference.max_abs,
                difference.bit_differences,
                difference.nonfinite[0],
                difference.nonfinite[1],
                difference.subnormal[0],
                difference.subnormal[1],
                difference.last_nonzero_frame[0],
                difference.last_nonzero_frame[1],
            );
        }
    }
}

pub(crate) fn bench(c: &mut Criterion) {
    verify_difference_accounting();
    let mut group = c.benchmark_group("crossover_df1_svf");
    let backend = if cfg!(any(target_os = "macos", target_os = "ios")) {
        "accelerate"
    } else {
        "portable"
    };
    for bands in [3, 10] {
        let frequencies = frequencies(bands);
        println!(
            "crossover_geometry rate={RATE} channels=2 bands={bands} frequencies={frequencies:?} backend={backend} target={}",
            std::env::consts::ARCH
        );
        for frames in [128, 512] {
            group.throughput(Throughput::Elements(
                u64::try_from(frames).expect("stereo frames"),
            ));
            let left = portable::eq_sine_440()[..frames].to_vec();
            let input = [
                left.clone(),
                left.iter().map(|sample| -0.75 * sample).collect(),
            ];
            let gains = gains(bands, frames, true);
            let mut baseline = Frame::new(&frequencies);
            group.bench_function(BenchmarkId::new(format!("df1_{bands}band"), frames), |b| {
                b.iter_batched_ref(
                    || input.clone(),
                    |io| {
                        baseline.sum(black_box(&mut *io), black_box(&gains), frames);
                        black_box(&*io);
                    },
                    BatchSize::SmallInput,
                );
            });
            diagnose(&frequencies, frames, "svf", || {
                Svf::new(&frequencies, frames)
            });
            let mut svf = Svf::new(&frequencies, frames);
            group.bench_function(BenchmarkId::new(format!("svf_{bands}band"), frames), |b| {
                b.iter_batched_ref(
                    || input.clone(),
                    |io| {
                        svf.sum(black_box(&mut *io), black_box(&gains), frames);
                        black_box(&*io);
                    },
                    BatchSize::SmallInput,
                );
            });
            for design in [Design::WidenedF32, Design::NativeF64] {
                diagnose(&frequencies, frames, &format!("biquad_{design:?}"), || {
                    Block::new(&frequencies, frames, design)
                });
                let mut candidate = Block::new(&frequencies, frames, design);
                group.bench_function(
                    BenchmarkId::new(format!("biquad_{design:?}_{bands}band"), frames),
                    |b| {
                        b.iter_batched_ref(
                            || input.clone(),
                            |io| {
                                candidate.sum(black_box(&mut *io), black_box(&gains), frames);
                                black_box(&*io);
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
            }
        }
    }
    group.finish();
}
