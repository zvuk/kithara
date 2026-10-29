//! Independent scalar EQ oracle with an expanded f32 recurrence.

mod filter {
    use kithara_bufpool::{HasPool, PoolError, PoolRegion, SampleBuffer};
    use kithara_dsp::filter::{Coefficients, Type};

    struct Section {
        coefficients: Coefficients<f32>,
        state: [f32; 4],
    }

    impl Section {
        const fn new(coefficients: Coefficients<f32>) -> Self {
            Self {
                coefficients,
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
                return 0.0;
            }
            output
        }
    }

    struct Lr4 {
        first: Section,
        second: Section,
    }

    impl Lr4 {
        const fn new(coefficients: Coefficients<f32>) -> Self {
            Self {
                first: Section::new(coefficients),
                second: Section::new(coefficients),
            }
        }

        #[inline]
        fn process(&mut self, input: f32) -> f32 {
            self.second.run(self.first.run(input))
        }
    }

    pub(crate) struct CrossoverFilters {
        crossover_freqs: SampleBuffer,
        lowpass_scratch: SampleBuffer,
        allpass: Vec<Section>,
        highpass: Vec<Lr4>,
        lowpass: Vec<Lr4>,
        sample_rate: f32,
    }

    impl CrossoverFilters {
        pub(crate) fn new<S>(
            pools: &PoolRegion<S>,
            crossover_freqs: SampleBuffer,
            sample_rate: f32,
        ) -> Result<Self, PoolError>
        where
            S: HasPool<f32>,
        {
            let lowpass = crossover_freqs
                .iter()
                .map(|&freq| Lr4::new(biquad_coeffs(Type::LowPass, freq, sample_rate)))
                .collect();
            let highpass = crossover_freqs
                .iter()
                .map(|&freq| Lr4::new(biquad_coeffs(Type::HighPass, freq, sample_rate)))
                .collect();
            let mut allpass: Vec<Section> = Vec::new();
            for start in 1..crossover_freqs.len() {
                allpass.extend(
                    crossover_freqs[start..]
                        .iter()
                        .map(|&freq| Section::new(biquad_coeffs(Type::AllPass, freq, sample_rate))),
                );
            }
            let lowpass_scratch = pools.get_with_len::<f32>(crossover_freqs.len())?;
            Ok(Self {
                crossover_freqs,
                lowpass_scratch,
                allpass,
                highpass,
                lowpass,
                sample_rate,
            })
        }

        pub(crate) fn process(&mut self, input: f32, gains: impl Fn(usize) -> f32) -> f32 {
            let mut high = input;
            for index in 0..self.lowpass.len() {
                self.lowpass_scratch[index] = self.lowpass[index].process(high);
                high = self.highpass[index].process(high);
            }
            let mut output = 0.0;
            let mut allpass_start = 0;
            for index in 0..self.lowpass.len() {
                let mut band = self.lowpass_scratch[index];
                let allpass_count = self.lowpass.len().saturating_sub(index + 1);
                let allpass_end = allpass_start + allpass_count;
                for filter in &mut self.allpass[allpass_start..allpass_end] {
                    band = filter.run(band);
                }
                allpass_start = allpass_end;
                output = band.mul_add(gains(index), output);
            }
            high.mul_add(gains(self.lowpass.len()), output)
        }

        fn rebuild(&mut self) {
            for (index, &freq) in self.crossover_freqs.iter().enumerate() {
                self.lowpass[index] =
                    Lr4::new(biquad_coeffs(Type::LowPass, freq, self.sample_rate));
                self.highpass[index] =
                    Lr4::new(biquad_coeffs(Type::HighPass, freq, self.sample_rate));
            }
            let mut allpass_start = 0;
            for band in 0..self.lowpass.len() {
                let allpass_count = self.lowpass.len().saturating_sub(band + 1);
                let allpass_end = allpass_start + allpass_count;
                for (offset, filter) in self.allpass[allpass_start..allpass_end]
                    .iter_mut()
                    .enumerate()
                {
                    let freq = self.crossover_freqs[band + 1 + offset];
                    *filter = Section::new(biquad_coeffs(Type::AllPass, freq, self.sample_rate));
                }
                allpass_start = allpass_end;
            }
        }

        pub(crate) fn reset(&mut self) {
            self.rebuild();
        }

        pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
            self.sample_rate = sample_rate;
            self.rebuild();
        }
    }

    fn biquad_coeffs(filter: Type<f32>, freq: f32, sample_rate: f32) -> Coefficients<f32> {
        let normalized = 2.0 * freq / sample_rate;
        Coefficients::<f32>::from_normalized_params(
            filter,
            normalized,
            std::f32::consts::FRAC_1_SQRT_2,
        )
        .unwrap_or(Coefficients {
            a1: 0.0,
            a2: 0.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
        })
    }
}
mod gain {
    use std::{num::NonZeroU32, ops::Index};

    use kithara_dsp::param::{
        DEFAULT_GAIN_SPAN, SmootherConfig, SmoothingFilter, SmoothingFilterCoeff,
    };
    use num_traits::cast::AsPrimitive;

    use crate::GainDb;

    #[derive(fieldwork::Fieldwork)]
    #[fieldwork(opt_in, get)]
    struct SmoothedGain {
        #[field(get(copy), vis = "pub(crate)")]
        target: GainDb,
        filter: SmoothingFilter,
        target_linear: f32,
    }

    impl SmoothedGain {
        fn new(target: GainDb) -> Self {
            let linear = target.linear();
            Self {
                target,
                filter: SmoothingFilter::new(linear),
                target_linear: linear,
            }
        }

        fn current(&self) -> f32 {
            self.filter.z1
        }

        fn set_target(&mut self, target: GainDb) {
            if target == self.target {
                return;
            }
            self.target = target;
            self.target_linear = target.linear();
        }

        #[inline]
        fn smooth(&mut self, coeff: SmoothingFilterCoeff, settle_ratio: f32) {
            self.filter.process(self.target_linear, coeff);
            self.filter
                .try_settle(self.target_linear, DEFAULT_GAIN_SPAN, settle_ratio);
        }
    }

    #[derive(fieldwork::Fieldwork)]
    #[fieldwork(opt_in, get)]
    pub(crate) struct GainBank {
        smoothing: SmootherConfig,
        coeff: SmoothingFilterCoeff,
        gains: Vec<SmoothedGain>,
    }

    impl GainBank {
        pub(crate) fn new(
            gains_db: impl Iterator<Item = GainDb>,
            sample_rate: f32,
            smoothing: SmootherConfig,
        ) -> Self {
            let gains = gains_db.map(SmoothedGain::new).collect();
            Self {
                gains,
                coeff: smoothing_coeff(sample_rate, smoothing),
                smoothing,
            }
        }

        pub(crate) fn reset(&mut self) {
            for gain in &mut self.gains {
                *gain = SmoothedGain::new(GainDb::default());
            }
        }

        pub(crate) fn set(&mut self, band: usize, gain_db: GainDb) {
            if let Some(gain) = self.gains.get_mut(band) {
                gain.set_target(gain_db);
            }
        }

        pub(crate) fn tick(&mut self) {
            for gain in &mut self.gains {
                gain.smooth(self.coeff, self.smoothing.settle_ratio);
            }
        }

        pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
            self.coeff = smoothing_coeff(sample_rate, self.smoothing);
        }

        delegate::delegate! {
            to self.gains {
                pub(crate) const fn len(&self) -> usize;
                #[expr($.current())]
                #[call(index)]
                pub(crate) fn linear(&self, band: usize) -> f32;
                #[expr($.map(SmoothedGain::target))]
                #[call(get)]
                pub(crate) fn target(&self, band: usize) -> Option<GainDb>;
            }
        }
    }

    fn smoothing_coeff(sample_rate: f32, smoothing: SmootherConfig) -> SmoothingFilterCoeff {
        let rate: u32 = sample_rate.max(1.0).as_();
        let rate = NonZeroU32::new(rate).unwrap_or(NonZeroU32::MIN);
        SmoothingFilterCoeff::new(rate, smoothing.smooth_seconds, smoothing.settle_ratio)
    }
}
use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP},
};
use kithara_signal::sanitize_sample;
use num_traits::cast::AsPrimitive;

use self::{filter::CrossoverFilters, gain::GainBank};
use crate::{
    GainDb,
    eq::{EqBandConfig, EqConfig},
};

/// Single-channel isolator crossover EQ.
#[non_exhaustive]
pub(super) struct IsolatorEq {
    filters: CrossoverFilters,
    gains: GainBank,
    bypass: MixDSP,
    silence: MixDSP,
}

impl IsolatorEq {
    /// Build a single-channel isolator over `bands`.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] when the region cannot hand out the crossover
    /// filter states and the gain bank this isolator runs on.
    pub(super) fn new<S>(
        config: &EqConfig<S>,
        bands: &[EqBandConfig],
        sample_rate: u32,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let rate = NonZeroU32::new(sample_rate).unwrap_or(NonZeroU32::MIN);
        let bypass = if bands.iter().all(|band| band.gain_db() == GainDb::default()) {
            Mix::FULLY_DRY
        } else {
            Mix::FULLY_WET
        };
        let silence = if !bands.is_empty() && bands.iter().all(|band| band.gain_db() == GainDb::MIN)
        {
            Mix::FULLY_DRY
        } else {
            Mix::FULLY_WET
        };
        let sample_rate: f32 = sample_rate.as_();
        let crossover_count = bands.len().saturating_sub(1);
        let mut crossover_freqs = config.pools().get_with_len::<f32>(crossover_count)?;
        for (frequency, pair) in crossover_freqs.iter_mut().zip(bands.windows(2)) {
            *frequency = (pair[0].frequency() * pair[1].frequency()).sqrt();
        }
        Ok(Self {
            bypass: MixDSP::new(bypass, FadeCurve::Linear, config.smoothing(), rate),
            silence: MixDSP::new(silence, FadeCurve::Linear, config.smoothing(), rate),
            filters: CrossoverFilters::new(config.pools(), crossover_freqs, sample_rate)?,
            gains: GainBank::new(
                bands.iter().map(EqBandConfig::gain_db),
                sample_rate,
                config.smoothing(),
            ),
        })
    }

    #[inline]
    pub(super) fn process_sample(&mut self, input: f32) -> f32 {
        let input = sanitize_sample(input);
        self.gains.tick();
        let mut output = [match self.gains.len() {
            0 => input,
            1 => sanitize_sample(input * self.gains.linear(0)),
            _ => sanitize_sample(self.filters.process(input, |band| self.gains.linear(band))),
        }];
        self.bypass.mix_dry_into_wet_mono(&[input], &mut output, 1);
        self.silence.mix_dry_into_wet_mono(&[0.0], &mut output, 1);
        sanitize_sample(output[0])
    }

    pub(super) fn reset(&mut self) {
        self.gains.reset();
        self.filters.reset();
        self.update_mix();
        self.bypass.reset_to_target();
        self.silence.reset_to_target();
    }

    pub(super) fn set_gain(&mut self, band: usize, gain_db: GainDb) {
        self.gains.set(band, gain_db);
        self.update_mix();
    }

    fn update_mix(&mut self) {
        let all =
            |target| (0..self.gains.len()).all(|band| self.gains.target(band) == Some(target));
        self.bypass.set_mix(
            if all(GainDb::default()) {
                Mix::FULLY_DRY
            } else {
                Mix::FULLY_WET
            },
            FadeCurve::Linear,
        );
        self.silence.set_mix(
            if self.gains.len() > 0 && all(GainDb::MIN) {
                Mix::FULLY_DRY
            } else {
                Mix::FULLY_WET
            },
            FadeCurve::Linear,
        );
    }

    pub(super) fn update_sample_rate(&mut self, sample_rate: u32) {
        let rate = NonZeroU32::new(sample_rate).unwrap_or(NonZeroU32::MIN);
        self.bypass.update_sample_rate(rate);
        self.silence.update_sample_rate(rate);
        let sample_rate = sample_rate.as_();
        self.gains.update_sample_rate(sample_rate);
        self.filters.update_sample_rate(sample_rate);
    }
}
