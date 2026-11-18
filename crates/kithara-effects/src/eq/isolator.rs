use std::num::NonZeroU32;

use kithara_bufpool::{HasPool, PoolError};
use kithara_dsp::{
    fade::FadeCurve,
    param::{Mix, MixDSP},
};
use kithara_signal::sanitize_sample;
use num_traits::cast::AsPrimitive;

use super::{EqBandConfig, EqConfig};
use crate::{
    GainDb,
    dsp::{
        filter::CrossoverFilters,
        gain::{GainBank, GainWrite},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoutingOwner {
    Gain(usize),
    Kill,
}

/// Single-channel isolator crossover EQ.
#[non_exhaustive]
pub struct IsolatorEq {
    filters: CrossoverFilters,
    gains: GainBank,
    bypass: MixDSP,
    silence: MixDSP,
    bypass_owner: Option<RoutingOwner>,
    silence_owner: Option<RoutingOwner>,
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
            bypass_owner: None,
            silence_owner: None,
            filters: CrossoverFilters::new(crossover_freqs, sample_rate),
            gains: GainBank::new(
                bands.iter().map(EqBandConfig::gain_db),
                sample_rate,
                config.smoothing(),
            ),
        })
    }

    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
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

    pub fn reset(&mut self) {
        self.gains.reset();
        self.reset_history();
    }

    fn reset_history(&mut self) {
        self.filters.reset();
        self.bypass_owner = None;
        self.silence_owner = None;
        self.update_mix(None);
        self.bypass.reset_to_target();
        self.silence.reset_to_target();
    }

    pub fn set_gain(&mut self, band: usize, gain_db: GainDb) {
        self.gains.set(band, gain_db);
        self.update_mix(Some(RoutingOwner::Gain(band)));
    }

    pub(super) fn set_kill(&mut self, band: usize, on: bool) {
        if self.gains.set_kill(band, on) {
            self.update_mix(Some(RoutingOwner::Kill));
        }
    }

    pub(super) fn clear_kills(&mut self) {
        if self.gains.clear_kills() {
            self.bypass_owner = None;
            self.silence_owner = None;
            self.update_mix(None);
            self.bypass.reset_to_target();
            self.silence.reset_to_target();
        }
    }

    pub(crate) fn set_explicit(&mut self, band: usize, gain_db: GainDb) {
        let owner = Some(RoutingOwner::Gain(band));
        let changed = match self.gains.set_explicit(band, gain_db) {
            GainWrite::Unchanged => false,
            GainWrite::Applied => {
                self.update_mix(owner);
                true
            }
            GainWrite::Gliding => {
                self.update_mix(Some(RoutingOwner::Kill));
                return;
            }
            GainWrite::Masked => return,
        };
        if (changed || self.bypass_owner == owner) && self.bypass_owner != Some(RoutingOwner::Kill)
        {
            self.bypass.reset_to_target();
            self.bypass_owner = None;
        }
        if (changed || self.silence_owner == owner)
            && self.silence_owner != Some(RoutingOwner::Kill)
        {
            self.silence.reset_to_target();
            self.silence_owner = None;
        }
    }

    fn update_mix(&mut self, owner: Option<RoutingOwner>) {
        let (bypass, silence) = self.gains.bypass_and_silence();
        update_route(&mut self.bypass, &mut self.bypass_owner, bypass, owner);
        update_route(&mut self.silence, &mut self.silence_owner, silence, owner);
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
            pub(super) fn killed(&self, band: usize) -> Option<bool>;
        }
    }
}

fn update_route(
    mix: &mut MixDSP,
    owner: &mut Option<RoutingOwner>,
    dry: bool,
    change: Option<RoutingOwner>,
) {
    let target = if dry { Mix::FULLY_DRY } else { Mix::FULLY_WET };
    if mix.second_gain_target() != target.get()
        && (*owner != Some(RoutingOwner::Kill) || mix.has_settled())
    {
        *owner = change;
    }
    mix.set_mix(target, FadeCurve::Linear);
}

#[cfg(test)]
mod tests {
    use kithara_dsp::param::SmoothingFilterCoeff;
    use kithara_test_fixtures::{portable, unit_fixtures::eq_impulse};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{pools, pools_with_budget};

    #[kithara::test]
    fn eq_kill_settled_release_preserves_program_takeover_during_a_manual_ramp() {
        let config = EqConfig::builder(pools()).build();
        let initial = GainDb::checked(-6.0).expect("initial gain");
        let band = EqBandConfig::builder().gain_db(initial).build();
        let mut released = IsolatorEq::new(&config, &[band], 48_000).expect("released band");
        let mut reference = IsolatorEq::new(&config, &[band], 48_000).expect("reference band");
        released.set_kill(0, true);
        for _ in 0..48_000 {
            released.process_sample(0.25);
        }
        assert_eq!(released.process_sample(0.25).to_bits(), 0);
        released.set_kill(0, false);
        for _ in 0..48_000 {
            released.process_sample(0.25);
            reference.process_sample(0.25);
        }
        let settled = (0.25 * initial.linear()).to_bits();
        assert_eq!(released.process_sample(0.25).to_bits(), settled);
        assert_eq!(reference.process_sample(0.25).to_bits(), settled);

        let manual = GainDb::checked(6.0).expect("manual gain");
        released.set_gain(0, manual);
        reference.set_gain(0, manual);
        for _ in 0..32 {
            let expected = reference.process_sample(0.25).to_bits();
            assert_ne!(expected, (0.25 * manual.linear()).to_bits());
            assert_eq!(released.process_sample(0.25).to_bits(), expected);
        }

        released.set_explicit(0, GainDb::DEFAULT);
        reference.set_explicit(0, GainDb::DEFAULT);
        let expected = reference.process_sample(0.25).to_bits();
        assert_eq!(expected, 0.25_f32.to_bits());
        assert_eq!(
            released.process_sample(0.25).to_bits(),
            expected,
            "program takeover frame"
        );
    }

    #[kithara::test]
    #[case::bypass(GainDb::DEFAULT, GainDb::MAX)]
    #[case::silence(GainDb::MIN, GainDb::DEFAULT)]
    fn eq_kill_routing_transition_is_not_snapped_by_another_band_program(
        #[case] initial: GainDb,
        #[case] program: GainDb,
    ) {
        let mut bands = super::super::band::generate_log_spaced_bands(2);
        bands[1].set_gain_db(initial);
        let config = EqConfig::builder(pools()).build();
        let mut eq = IsolatorEq::new(&config, &bands, 48_000).expect("isolator");
        let settled = |eq: &IsolatorEq| {
            if initial == GainDb::MIN {
                eq.silence.has_settled()
            } else {
                eq.bypass.has_settled()
            }
        };
        assert!(settled(&eq));
        eq.set_kill(0, true);
        for _ in 0..128 {
            assert!(eq.process_sample(0.25).is_finite());
        }
        assert!(!settled(&eq));
        eq.set_explicit(1, program);
        assert!(!settled(&eq));
        assert!(eq.process_sample(0.25).is_finite());
    }

    #[kithara::test]
    #[case::slow(16_000, 1.0, true)]
    #[case::steep(4_000, 12.0, false)]
    fn eq_kill_off_regains_exact_program_tracking_after_the_smoother_settles(
        #[case] frames: u16,
        #[case] span: f32,
        #[case] settles_while_running: bool,
    ) {
        let initial = GainDb::checked(-6.0).expect("initial gain");
        let config = EqConfig::builder(pools()).build();
        let band = EqBandConfig::builder().gain_db(initial).build();
        let mut eq = IsolatorEq::new(&config, &[band], 48_000).expect("isolator");
        eq.set_kill(0, true);
        for _ in 0..48_000 {
            eq.process_sample(0.25);
        }
        assert_eq!(eq.process_sample(0.25).to_bits(), 0);
        eq.set_kill(0, false);
        let mut tracked = false;
        for frame in 0..frames {
            let gain = GainDb::checked(-6.0 + span * f32::from(frame) / f32::from(frames))
                .expect("program gain");
            eq.set_explicit(0, gain);
            let pcm = eq.process_sample(0.25);
            let expected = 0.25 * gain.linear();
            assert!(pcm.is_finite());
            assert_eq!(eq.target_gain(0), Some(gain));
            let exact = pcm.to_bits() == expected.to_bits();
            if frame == 0 {
                assert!(pcm > 0.0 && pcm < expected);
            }
            assert!(!tracked || exact);
            tracked |= exact;
        }
        assert_eq!(tracked, settles_while_running);
        let held = GainDb::checked(-6.0 + span).expect("held gain");
        for _ in 0..48_000 {
            eq.set_explicit(0, held);
            eq.process_sample(0.25);
        }
        assert_eq!(
            eq.process_sample(0.25).to_bits(),
            (0.25 * held.linear()).to_bits()
        );
        eq.set_explicit(0, initial);
        assert_eq!(
            eq.process_sample(0.25).to_bits(),
            (0.25 * initial.linear()).to_bits()
        );
    }

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
