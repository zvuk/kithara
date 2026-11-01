use std::hint::black_box;

use biquad::{Biquad, Coefficients, DirectForm1, Type};
use criterion::{BenchmarkId, Criterion, Throughput};

struct Section(DirectForm1<f32>);

impl Section {
    fn new(kind: Type<f32>, frequency: f32) -> Self {
        Self(DirectForm1::new(
            Coefficients::from_normalized_params(
                kind,
                2.0 * frequency / 48_000.0,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .unwrap_or(Coefficients {
                a1: 0.0,
                a2: 0.0,
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
            }),
        ))
    }

    #[inline]
    fn run(&mut self, input: f32) -> f32 {
        let output = self.0.run(input);
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.0.reset_state();
            0.0
        } else {
            output
        }
    }
}

struct Lr4 {
    first: Section,
    second: Section,
}

impl Lr4 {
    fn new(kind: Type<f32>, frequency: f32) -> Self {
        Self {
            first: Section::new(kind, frequency),
            second: Section::new(kind, frequency),
        }
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        self.second.run(self.first.run(input))
    }
}

/// The pre-migration frame-first law, writing bands before gain accumulation.
pub(super) struct Original {
    low: Vec<Lr4>,
    high: Vec<Lr4>,
    scratch: Vec<f32>,
    allpass: Vec<Section>,
}

impl Original {
    pub(super) fn new(frequencies: &[f32]) -> Self {
        Self {
            low: frequencies
                .iter()
                .map(|&frequency| Lr4::new(Type::LowPass, frequency))
                .collect(),
            high: frequencies
                .iter()
                .map(|&frequency| Lr4::new(Type::HighPass, frequency))
                .collect(),
            scratch: vec![0.0; frequencies.len()],
            allpass: (1..frequencies.len())
                .flat_map(|start| {
                    frequencies[start..]
                        .iter()
                        .map(|&frequency| Section::new(Type::AllPass, frequency))
                })
                .collect(),
        }
    }

    pub(super) fn split(&mut self, input: &[f32], bands: &mut [Vec<f32>]) -> usize {
        let count = self.low.len() + 1;
        let Some(bands) = bands.get_mut(..count) else {
            return 0;
        };
        let frames = bands.iter().fold(input.len(), |n, band| n.min(band.len()));
        for (frame, &sample) in input.iter().take(frames).enumerate() {
            let mut high = sample;
            for index in 0..self.low.len() {
                self.scratch[index] = self.low[index].process(high);
                high = self.high[index].process(high);
            }
            let mut allpass_start = 0;
            for index in 0..self.low.len() {
                let mut band = self.scratch[index];
                let allpass_end = allpass_start + self.low.len().saturating_sub(index + 1);
                for section in &mut self.allpass[allpass_start..allpass_end] {
                    band = section.run(band);
                }
                allpass_start = allpass_end;
                bands[index][frame] = band;
            }
            bands[self.low.len()][frame] = high;
        }
        frames
    }
}

pub(super) fn bench<F, R>(c: &mut Criterion, make: F)
where
    F: Fn(&[f32]) -> R,
    R: FnMut(&[f32], &mut [Vec<f32>]) -> usize,
{
    let mut group = c.benchmark_group("crossover");
    for frames in [7, 128, 255, 256, 257, 1024] {
        group.throughput(Throughput::Elements(
            u64::try_from(frames).expect("frame count fits u64"),
        ));
        for frequencies in [
            &[250.0, 2_500.0][..],
            &[
                100.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 6_000.0, 9_000.0, 12_000.0,
            ][..],
        ] {
            let mut split = make(frequencies);
            let mut bands = vec![vec![0.0; frames]; frequencies.len() + 1];
            for (label, input) in [
                ("tone", super::tone(frames)),
                ("quiet", vec![f32::MIN_POSITIVE; frames]),
                ("pathological", vec![f32::MAX; frames]),
            ] {
                group.bench_with_input(
                    BenchmarkId::new(format!("{}band_{label}", bands.len()), frames),
                    &frames,
                    |b, _| {
                        b.iter(|| {
                            let count = split(black_box(&input), black_box(&mut bands));
                            black_box(&bands);
                            count
                        });
                    },
                );
            }
        }
    }
    group.finish();
}
