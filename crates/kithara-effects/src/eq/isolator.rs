use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError, SampleBuffer};
use kithara_dsp::{
    fade::FadeCurve,
    filter::Crossover,
    param::{Mix, MixDSP},
};
use kithara_signal::sanitize_sample;
use num_traits::cast::AsPrimitive;

use super::{EqBandConfig, EqConfig, gain::GainBank};
use crate::{GainDb, consts::EQ_BLOCK_FRAMES};

/// Single-channel isolator crossover EQ.
#[non_exhaustive]
pub struct IsolatorEq {
    filters: Crossover,
    bands: Vec<SampleBuffer>,
    gain_blocks: Vec<SampleBuffer>,
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
    pub fn new<S>(
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
            filters: Crossover::new(&crossover_freqs, sample_rate),
            bands: (0..if bands.len() > 1 { bands.len() } else { 0 })
                .map(|_| config.pools().get_with_len::<f32>(EQ_BLOCK_FRAMES))
                .collect::<Result<_, _>>()?,
            gain_blocks: (0..bands.len())
                .map(|_| config.pools().get_with_len::<f32>(EQ_BLOCK_FRAMES))
                .collect::<Result<_, _>>()?,
            gains: GainBank::new(
                bands.iter().map(EqBandConfig::gain_db),
                sample_rate,
                config.smoothing(),
            ),
        })
    }

    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let mut output = [0.0];
        self.process_block::<1>(&[input], &mut output);
        output[0]
    }

    /// Processes the common input/output prefix; the remaining output stays untouched.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        let count = input.len().min(output.len());
        for (input, output) in input[..count]
            .chunks(EQ_BLOCK_FRAMES)
            .zip(output[..count].chunks_mut(EQ_BLOCK_FRAMES))
        {
            self.process_block::<EQ_BLOCK_FRAMES>(input, output);
        }
        count
    }

    fn process_block<const N: usize>(&mut self, input: &[f32], output: &mut [f32]) {
        let frames = input.len();
        let mut dry = [0.0; N];
        for (dry, &input) in dry.iter_mut().zip(input) {
            *dry = sanitize_sample(input);
        }
        self.gains.fill(&mut self.gain_blocks, frames);
        let mut wet = [0.0; N];
        match self.gains.len() {
            0 => wet[..frames].copy_from_slice(&dry[..frames]),
            1 => {
                for ((wet, dry), gain) in wet
                    .iter_mut()
                    .zip(dry)
                    .zip(self.gain_blocks[0].iter())
                    .take(frames)
                {
                    *wet = dry * gain;
                }
            }
            _ => {
                self.filters.split(&dry[..frames], &mut self.bands);
                for (band, gains) in self.bands.iter().zip(&self.gain_blocks) {
                    kithara_dsp::mul_add(&band[..frames], &gains[..frames], &mut wet[..frames]);
                }
            }
        }
        for ((output, dry), wet) in output.iter_mut().zip(dry).zip(wet) {
            let mut wet = [sanitize_sample(wet)];
            self.bypass.mix_dry_into_wet_mono(&[dry], &mut wet, 1);
            self.silence.mix_dry_into_wet_mono(&[0.0], &mut wet, 1);
            *output = sanitize_sample(wet[0]);
        }
    }

    pub fn reset(&mut self) {
        self.gains.reset();
        self.filters.reset();
        self.update_mix();
        self.bypass.reset_to_target();
        self.silence.reset_to_target();
    }

    pub fn set_gain(&mut self, band: usize, gain_db: GainDb) {
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

    pub fn update_sample_rate(&mut self, sample_rate: u32) {
        let rate = NonZeroU32::new(sample_rate).unwrap_or(NonZeroU32::MIN);
        self.bypass.update_sample_rate(rate);
        self.silence.update_sample_rate(rate);
        let sample_rate = sample_rate.as_();
        self.gains.update_sample_rate(sample_rate);
        self.filters.update_sample_rate(sample_rate);
    }

    delegate::delegate! {
        to self.gains {
            #[must_use]
            #[call(len)]
            pub const fn band_count(&self) -> usize;
            #[must_use]
            #[call(target)]
            pub fn target_gain(&self, band: usize) -> Option<GainDb>;
            #[cfg(test)]
            pub(crate) fn is_smoothing(&self) -> bool;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_dsp::param::SmoothingFilterCoeff;
    use kithara_test_fixtures::{portable, unit_fixtures::eq_impulse};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{pools, pools_with_budget};

    #[kithara::test]
    fn a_decaying_tail_never_leaks_denormals(eq_impulse: Vec<f32>) {
        const SAMPLE_RATE: u32 = 48_000;

        let bands = super::super::band::generate_log_spaced_bands(3);
        let config = EqConfig::builder(pools()).build();
        let mut eq = IsolatorEq::new(&config, &bands, SAMPLE_RATE)
            .unwrap_or_else(|error| panic!("test isolator: {error}"));
        for band in 0..bands.len() {
            eq.set_gain(band, GainDb::MAX);
        }

        let _ = eq.process_sample(eq_impulse[0]);
        let denormals = eq_impulse[1..]
            .iter()
            .copied()
            .map(|sample| eq.process_sample(sample))
            .filter(|out| *out != 0.0 && out.abs() < f32::MIN_POSITIVE)
            .count();

        assert_eq!(denormals, 0, "impulse tail leaked {denormals} denormals");
    }

    /// A gain move from unity is a ramp at the sample rate: no output sample
    /// steps by more than the tone's own slope plus the smoother's per-sample
    /// share. That share is the smoothing filter's first step over the whole
    /// gain range, which is what a move from unity to silence asks of it.
    #[kithara::test]
    fn a_gain_move_from_unity_never_steps() {
        const SAMPLE_RATE: u32 = 48_000;
        const TONE_HZ: f32 = 440.0;
        const CHANGE_AT: usize = 480;
        let bands = super::super::band::generate_log_spaced_bands(3);
        let config = EqConfig::builder(pools()).build();
        let mut eq = IsolatorEq::new(&config, &bands, SAMPLE_RATE)
            .unwrap_or_else(|error| panic!("test isolator: {error}"));
        let rate: f32 = SAMPLE_RATE.as_();
        let tone = portable::eq_sine_440();
        let mut previous = eq.process_sample(tone[0]);
        let mut max_step = 0.0_f32;
        for (n, sample) in tone.iter().enumerate().skip(1) {
            if n == CHANGE_AT {
                eq.set_gain(1, GainDb::MIN);
            }
            let out = eq.process_sample(*sample);
            if n >= CHANGE_AT / 2 {
                max_step = max_step.max((out - previous).abs());
            }
            previous = out;
        }
        let slope = TONE_HZ * std::f32::consts::TAU / rate;
        let smoothing = config.smoothing();
        let rate = NonZeroU32::new(SAMPLE_RATE).expect("static sample rate is non-zero");
        let coeff =
            SmoothingFilterCoeff::new(rate, smoothing.smooth_seconds, smoothing.settle_ratio);
        let ramp = coeff.a0;
        assert!(
            max_step <= slope + ramp,
            "a gain move stepped the output: {max_step} > {slope} + {ramp}"
        );
    }

    #[kithara::test]
    fn reusable_storage_returns_to_the_injected_pool() {
        let pools = pools_with_budget(1024 * 1024);
        let bands = super::super::band::generate_log_spaced_bands(3);
        let config = EqConfig::builder(pools.clone()).build();

        let first = IsolatorEq::new(&config, &bands, 48_000)
            .unwrap_or_else(|error| panic!("first isolator: {error}"));
        drop(first);
        let allocated = pools.stats().allocated_bytes;

        let _second = IsolatorEq::new(&config, &bands, 48_000)
            .unwrap_or_else(|error| panic!("second isolator: {error}"));

        assert_eq!(pools.stats().allocated_bytes, allocated);
    }
}
