use std::{hint::black_box, num::NonZeroU32, ops::Deref};

use biquad::{Biquad as _, DirectForm1};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use kithara_dsp::{
    filter::{Coefficients, Crossover, Hertz, Type},
    param::{DEFAULT_GAIN_SPAN, MIN_SETTLE_RATIO, SmoothingFilter, SmoothingFilterCoeff},
};
use kithara_test_fixtures::portable;
use kithara_test_utils::kithara_platform::time::Duration;
use num_traits::AsPrimitive;

const RATE: f32 = 48_000.0;
/// Peak below which the crossover treats a sample as silent; the quiet-noise
/// input stays well under it.
const SILENT_PEAK: f32 = 1.0e-9;

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

struct Df1Section(DirectForm1<f32>);

impl Df1Section {
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

struct ScalarSplit {
    low: [Df1Section; 2],
    high: [Df1Section; 2],
    allpass: Vec<Df1Section>,
}

const SCALAR_GAIN_FRAMES: usize = 128;

struct Scalar {
    splits: Vec<ScalarSplit>,
    frame: Vec<f32>,
    gain_tiles: Vec<[f32; SCALAR_GAIN_FRAMES]>,
}

impl Scalar {
    fn new(frequencies: &[f32]) -> Self {
        Self {
            splits: frequencies
                .iter()
                .enumerate()
                .map(|(index, frequency)| ScalarSplit {
                    low: std::array::from_fn(|_| Df1Section::new(Type::LowPass, *frequency)),
                    high: std::array::from_fn(|_| Df1Section::new(Type::HighPass, *frequency)),
                    allpass: frequencies[index + 1..]
                        .iter()
                        .map(|frequency| Df1Section::new(Type::AllPass, *frequency))
                        .collect(),
                })
                .collect(),
            frame: vec![0.0; frequencies.len() + 1],
            gain_tiles: vec![[0.0; SCALAR_GAIN_FRAMES]; frequencies.len() + 1],
        }
    }

    fn sum<P: Deref<Target = [f32]>>(&mut self, samples: &mut [f32], gains: &[P]) {
        let count = samples.len();
        for (chunk, start) in samples
            .chunks_mut(SCALAR_GAIN_FRAMES)
            .zip((0..count).step_by(SCALAR_GAIN_FRAMES))
        {
            for (tile, gain) in self.gain_tiles.iter_mut().zip(gains) {
                tile[..chunk.len()].copy_from_slice(&gain[start..start + chunk.len()]);
            }
            self.weighted_block(chunk);
        }
    }

    #[inline(never)]
    fn weighted_block(&mut self, samples: &mut [f32]) {
        for (sample, frame) in samples.iter_mut().zip(0..SCALAR_GAIN_FRAMES) {
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

struct Frame([Scalar; 2]);

impl Frame {
    fn new(frequencies: &[f32]) -> Self {
        Self(std::array::from_fn(|_| Scalar::new(frequencies)))
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

type SplitKernel = fn(&mut Crossover<2>, &[&[f32]; 2], &mut [[Vec<f32>; 2]]) -> usize;

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

    fn sum_with<P: Deref<Target = [f32]>>(
        &mut self,
        io: &mut [Vec<f32>; 2],
        gains: &[P],
        frames: usize,
        split: SplitKernel,
    ) {
        let input = io.each_ref().map(|plane| &plane[..frames]);
        assert_eq!(
            black_box(split)(&mut self.filter, &input, &mut self.bands),
            frames
        );
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

fn input_forms(frames: usize) -> [(&'static str, [Vec<f32>; 2]); 3] {
    let stereo = |left: Vec<f32>| {
        let right = left.iter().map(|sample| -0.75 * sample).collect();
        [left, right]
    };
    let mut state = 0x9e37_79b9_u32;
    let noise = std::iter::repeat_with(|| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let value = f32::from(u16::try_from(state >> 16).expect("upper noise bits"));
        ((value + 0.5) / 65_536.0 - 0.5) * (SILENT_PEAK * 0.125)
    })
    .take(frames)
    .collect();
    [
        ("signal", stereo(portable::eq_sine_440()[..frames].to_vec())),
        ("silence", std::array::from_fn(|_| vec![0.0; frames])),
        ("quiet_noise", stereo(noise)),
    ]
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

pub(crate) fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("crossover_df1_svf");
    for bands in [3, 10] {
        let frequencies = frequencies(bands);
        for frames in [128, 512] {
            group.throughput(Throughput::Elements(
                u64::try_from(frames).expect("stereo frames"),
            ));
            let (samples, seconds) = if bands == 3 && frames == 512 {
                (100, 20)
            } else {
                (20, 5)
            };
            group
                .sample_size(samples)
                .measurement_time(Duration::from_secs(seconds));
            let forms = input_forms(frames);
            let gains = gains(bands, frames, true);
            for (shape, input) in &forms {
                let variant = if *shape == "signal" {
                    format!("{bands}band")
                } else {
                    format!("{shape}_{bands}band")
                };
                let mut baseline = Frame::new(&frequencies);
                group.bench_function(BenchmarkId::new(format!("df1_{variant}"), frames), |b| {
                    b.iter_batched_ref(
                        || (*input).clone(),
                        |io| {
                            baseline.sum(black_box(&mut *io), black_box(&gains), frames);
                            black_box(&*io);
                        },
                        BatchSize::SmallInput,
                    );
                });
                for (name, kernel) in kithara_dsp::routes::crossover::forms::<2, Vec<f32>>() {
                    let mut svf = Svf::new(&frequencies, frames);
                    group.bench_function(
                        BenchmarkId::new(format!("{name}_{variant}"), frames),
                        |b| {
                            b.iter_batched_ref(
                                || (*input).clone(),
                                |io| {
                                    svf.sum_with(
                                        black_box(&mut *io),
                                        black_box(&gains),
                                        frames,
                                        kernel,
                                    );
                                    black_box(&*io);
                                },
                                BatchSize::SmallInput,
                            );
                        },
                    );
                }
            }
        }
    }
    group.finish();
}
