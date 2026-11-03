use std::num::NonZeroU32;

use kithara::{
    effects::{GainDb, eq::EqBandConfig},
    signal::sanitize_sample,
};
use kithara_dsp::{
    fade::FadeCurve,
    filter::{Coefficients, Type},
    param::{
        DEFAULT_GAIN_SPAN, Mix, MixDSP, SmootherConfig, SmoothingFilter, SmoothingFilterCoeff,
    },
};

fn linear(gain: GainDb) -> f32 {
    if gain == GainDb::MIN {
        0.0
    } else {
        10.0_f32.powf(f32::from(gain) / 20.0)
    }
}

struct Section {
    coefficients: Coefficients<f32>,
    state: [f32; 4],
}

impl Section {
    fn new(kind: Type<f32>, frequency: f32, rate: f32) -> Self {
        Self {
            coefficients: Coefficients::from_normalized_params(
                kind,
                2.0 * frequency / rate,
                std::f32::consts::FRAC_1_SQRT_2,
            )
            .expect("valid benchmark crossover"),
            state: [0.0; 4],
        }
    }

    fn run(&mut self, input: f32) -> f32 {
        let [x1, x2, y1, y2] = self.state;
        let c = self.coefficients;
        let output = c.b0 * input + c.b1 * x1 + c.b2 * x2 - c.a1 * y1 - c.a2 * y2;
        self.state = [input, x1, output, y1];
        if output.is_subnormal() && input.abs() < f32::MIN_POSITIVE {
            self.state = [0.0; 4];
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
    fn new(kind: Type<f32>, frequency: f32, rate: f32) -> Self {
        Self {
            first: Section::new(kind, frequency, rate),
            second: Section::new(kind, frequency, rate),
        }
    }

    fn run(&mut self, input: f32) -> f32 {
        self.second.run(self.first.run(input))
    }
}

/// The original frame-first EQ law, independent of production crossover processing.
pub(super) struct EqReference {
    low: Vec<Lr4>,
    high: Vec<Lr4>,
    allpass: Vec<Section>,
    scratch: Vec<f32>,
    gains: Vec<SmoothingFilter>,
    targets: Vec<f32>,
    coeff: SmoothingFilterCoeff,
    smoothing: SmootherConfig,
    bypass: MixDSP,
    silence: MixDSP,
}

impl EqReference {
    pub(super) fn new(
        bands: &[EqBandConfig],
        targets: &[GainDb],
        rate: NonZeroU32,
        smoothing: SmootherConfig,
    ) -> Self {
        assert!(bands.len() > 1);
        assert_eq!(bands.len(), targets.len());
        assert!(bands.iter().all(|band| band.gain_db() == GainDb::default()));
        assert!(targets.iter().any(|gain| *gain != GainDb::default()));
        assert!(targets.iter().any(|gain| *gain != GainDb::MIN));
        let frequencies: Vec<_> = bands
            .windows(2)
            .map(|pair| (pair[0].frequency() * pair[1].frequency()).sqrt())
            .collect();
        let sample_rate = num_traits::cast::AsPrimitive::<f32>::as_(rate.get());
        let mut bypass = MixDSP::new(Mix::FULLY_DRY, FadeCurve::Linear, smoothing, rate);
        bypass.set_mix(Mix::FULLY_WET, FadeCurve::Linear);
        Self {
            low: frequencies
                .iter()
                .map(|&hz| Lr4::new(Type::LowPass, hz, sample_rate))
                .collect(),
            high: frequencies
                .iter()
                .map(|&hz| Lr4::new(Type::HighPass, hz, sample_rate))
                .collect(),
            allpass: (1..frequencies.len())
                .flat_map(|start| {
                    frequencies[start..]
                        .iter()
                        .map(|&hz| Section::new(Type::AllPass, hz, sample_rate))
                })
                .collect(),
            scratch: vec![0.0; frequencies.len()],
            gains: bands
                .iter()
                .map(|_| SmoothingFilter::new(linear(GainDb::default())))
                .collect(),
            targets: targets.iter().map(|gain| linear(*gain)).collect(),
            coeff: SmoothingFilterCoeff::new(
                rate,
                smoothing.smooth_seconds,
                smoothing.settle_ratio,
            ),
            smoothing,
            bypass,
            silence: MixDSP::new(Mix::FULLY_WET, FadeCurve::Linear, smoothing, rate),
        }
    }

    pub(super) fn assert_settled(&self) {
        for (gain, target) in self.gains.iter().zip(&self.targets) {
            assert_eq!(
                gain.z1.to_bits(),
                target.to_bits(),
                "benchmark gain must settle"
            );
        }
        assert!(self.bypass.has_settled());
        assert!(self.silence.has_settled());
    }

    pub(super) fn sample(&mut self, input: f32) -> f32 {
        let input = sanitize_sample(input);
        for (gain, &target) in self.gains.iter_mut().zip(&self.targets) {
            gain.process(target, self.coeff);
            gain.try_settle(target, DEFAULT_GAIN_SPAN, self.smoothing.settle_ratio);
        }
        let mut high = input;
        for index in 0..self.low.len() {
            self.scratch[index] = self.low[index].run(high);
            high = self.high[index].run(high);
        }
        let mut output = 0.0;
        let mut start = 0;
        for index in 0..self.low.len() {
            let mut band = self.scratch[index];
            let end = start + self.low.len().saturating_sub(index + 1);
            for section in &mut self.allpass[start..end] {
                band = section.run(band);
            }
            start = end;
            output = band.mul_add(self.gains[index].z1, output);
        }
        let mut wet = [sanitize_sample(
            high.mul_add(self.gains[self.low.len()].z1, output),
        )];
        self.bypass.mix_dry_into_wet_mono(&[input], &mut wet, 1);
        self.silence.mix_dry_into_wet_mono(&[0.0], &mut wet, 1);
        sanitize_sample(wet[0])
    }
}
