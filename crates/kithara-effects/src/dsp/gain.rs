use std::{num::NonZeroU32, ops::DerefMut};

use kithara_dsp::param::{
    DEFAULT_GAIN_SPAN, SmootherConfig, SmoothingFilter, SmoothingFilterCoeff,
};
use num_traits::cast::AsPrimitive;

use crate::GainDb;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kill {
    Off,
    On,
    Releasing,
}

pub(crate) enum GainWrite {
    Unchanged,
    Applied,
    Gliding,
    Masked,
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
struct SmoothedGain {
    #[field(get(copy), vis = "pub(crate)")]
    target: GainDb,
    filter: SmoothingFilter,
    target_linear: f32,
    kill: Kill,
}

impl SmoothedGain {
    fn new(target: GainDb) -> Self {
        let linear = target.linear();
        Self {
            target,
            filter: SmoothingFilter::new(linear),
            target_linear: linear,
            kill: Kill::Off,
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
        self.update_linear_target();
    }

    fn update_linear_target(&mut self) {
        self.target_linear = if self.kill == Kill::On {
            0.0
        } else {
            self.target.linear()
        };
    }

    fn set_explicit(&mut self, target: GainDb) -> GainWrite {
        match self.kill {
            Kill::On => {
                self.target = target;
                return GainWrite::Masked;
            }
            Kill::Releasing if !self.filter.has_settled(self.target_linear) => {
                if self.target == target {
                    return GainWrite::Masked;
                }
                self.set_target(target);
                return GainWrite::Gliding;
            }
            Kill::Releasing => self.kill = Kill::Off,
            Kill::Off => {}
        }
        if self.target == target && self.current() == self.target_linear {
            GainWrite::Unchanged
        } else {
            *self = Self::new(target);
            GainWrite::Applied
        }
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
            if gain.kill == Kill::Releasing && gain.filter.has_settled(gain.target_linear) {
                gain.kill = Kill::Off;
            }
            gain.set_target(gain_db);
        }
    }

    pub(crate) fn tick(&mut self) {
        for gain in &mut self.gains {
            gain.smooth(self.coeff, self.smoothing.settle_ratio);
        }
    }

    pub(crate) fn fill<B: DerefMut<Target = [f32]>>(&mut self, blocks: &mut [B], frames: usize) {
        for frame in 0..frames {
            self.tick();
            for (gain, block) in self.gains.iter().zip(blocks.iter_mut()) {
                block[frame] = gain.current();
            }
        }
    }

    pub(crate) fn set_explicit(&mut self, band: usize, gain_db: GainDb) -> GainWrite {
        self.gains
            .get_mut(band)
            .map_or(GainWrite::Unchanged, |gain| gain.set_explicit(gain_db))
    }

    pub(crate) fn set_kill(&mut self, band: usize, on: bool) -> bool {
        let Some(gain) = self.gains.get_mut(band) else {
            return false;
        };
        if (gain.kill == Kill::On) == on {
            return false;
        }
        gain.kill = if on { Kill::On } else { Kill::Releasing };
        gain.update_linear_target();
        true
    }

    pub(crate) fn clear_kills(&mut self) -> bool {
        let mut changed = false;
        for gain in &mut self.gains {
            if gain.kill != Kill::Off {
                *gain = SmoothedGain::new(gain.target);
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn bypass_and_silence(&self) -> (bool, bool) {
        let mut bypass = true;
        let mut silence = !self.gains.is_empty();
        for gain in &self.gains {
            let killed = gain.kill == Kill::On;
            bypass &= !killed && gain.target == GainDb::DEFAULT;
            silence &= killed || gain.target == GainDb::MIN;
            if !bypass && !silence {
                break;
            }
        }
        (bypass, silence)
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
        self.coeff = smoothing_coeff(sample_rate, self.smoothing);
    }

    delegate::delegate! {
        to self.gains {
            pub(crate) const fn len(&self) -> usize;
            #[expr($.map(SmoothedGain::target))]
            #[call(get)]
            pub(crate) fn target(&self, band: usize) -> Option<GainDb>;
            #[expr($.map(|gain| gain.kill == Kill::On))]
            #[call(get)]
            pub(crate) fn killed(&self, band: usize) -> Option<bool>;
        }
    }
}

fn smoothing_coeff(sample_rate: f32, smoothing: SmootherConfig) -> SmoothingFilterCoeff {
    let rate: u32 = sample_rate.max(1.0).as_();
    let rate = NonZeroU32::new(rate).unwrap_or(NonZeroU32::MIN);
    SmoothingFilterCoeff::new(rate, smoothing.smooth_seconds, smoothing.settle_ratio)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    fn linear(bank: &GainBank, band: usize) -> f32 {
        bank.gains[band].current()
    }

    /// `IsolatorEq::new` takes a plain `u32`, so a caller can hand the bank a
    /// rate no filter can be built from. The band still has to arrive.
    #[kithara::test]
    fn a_bank_built_at_an_unusable_sample_rate_still_reaches_its_target() {
        let smoothing = SmootherConfig {
            smooth_seconds: 0.01,
            settle_ratio: 0.0001,
        };
        let mut bank = GainBank::new([GainDb::default()].into_iter(), 0.0, smoothing);
        bank.set(0, GainDb::MAX);

        for _ in 0..1 << 12 {
            bank.tick();
        }

        assert_eq!(linear(&bank, 0), GainDb::MAX.linear());
    }

    #[kithara::test]
    fn explicit_values_do_not_restart_an_unrelated_manual_smoother() {
        let mut bank = GainBank::new(
            [GainDb::DEFAULT; 2].into_iter(),
            44_100.0,
            SmootherConfig::default(),
        );
        let mut reference = GainBank::new(
            [GainDb::DEFAULT; 2].into_iter(),
            44_100.0,
            SmootherConfig::default(),
        );
        bank.set(1, GainDb::MAX);
        reference.set(1, GainDb::MAX);
        for value in [GainDb::MIN, GainDb::DEFAULT, GainDb::MAX]
            .into_iter()
            .cycle()
            .take(64)
        {
            bank.set_explicit(0, value);
            bank.tick();
            reference.tick();
            assert_eq!(linear(&bank, 0).to_bits(), value.linear().to_bits());
            assert_eq!(linear(&bank, 1).to_bits(), linear(&reference, 1).to_bits());
            assert_eq!(bank.target(1), reference.target(1));
        }
    }

    #[kithara::test]
    fn kill_keeps_the_program_value_while_the_band_reaches_exact_zero() {
        let mut bank = GainBank::new(
            [GainDb::MAX].into_iter(),
            48_000.0,
            SmootherConfig::default(),
        );
        assert!(bank.set_kill(0, true));
        bank.set_explicit(0, GainDb::DEFAULT);
        bank.tick();
        assert_eq!(bank.target(0), Some(GainDb::DEFAULT));
        assert!(linear(&bank, 0) > 0.0);
        assert!(linear(&bank, 0) < GainDb::MAX.linear());
        assert!(!bank.set_kill(0, true));
        for _ in 0..48_000 {
            bank.tick();
        }
        assert_eq!(linear(&bank, 0).to_bits(), 0);
        assert!(bank.set_kill(0, false));
        assert_eq!(linear(&bank, 0).to_bits(), 0);
        bank.tick();
        assert!(linear(&bank, 0) > 0.0 && linear(&bank, 0) < 1.0);
    }

    #[kithara::test]
    fn filled_blocks_match_frame_by_frame_ticks_and_final_settlement() {
        let smoothing = SmootherConfig {
            smooth_seconds: 1.0,
            settle_ratio: 0.25,
        };
        let mut actual = GainBank::new([GainDb::DEFAULT; 2].into_iter(), 2.0, smoothing);
        let mut reference = GainBank::new([GainDb::DEFAULT; 2].into_iter(), 2.0, smoothing);
        actual.set(0, GainDb::MAX);
        reference.set(0, GainDb::MAX);
        assert!(actual.set_kill(1, true));
        assert!(reference.set_kill(1, true));
        let mut blocks = vec![vec![0.0; 256]; 2];
        let mut expected = [Vec::new(), Vec::new()];

        for frames in [1, 7, 255] {
            for _ in 0..frames {
                reference.tick();
                for (band, expected) in expected.iter_mut().enumerate() {
                    expected.push(linear(&reference, band));
                }
            }
            actual.fill(&mut blocks, frames);
            for (band, expected) in expected.iter().enumerate() {
                assert_eq!(blocks[band][..frames], expected[expected.len() - frames..]);
                assert_eq!(
                    linear(&actual, band).to_bits(),
                    linear(&reference, band).to_bits()
                );
            }
        }
        assert_eq!(linear(&actual, 0).to_bits(), GainDb::MAX.linear().to_bits());
        assert_eq!(linear(&actual, 1).to_bits(), 0);
    }

    #[kithara::test]
    fn eq_kill_keeps_the_strict_settle_boundary() {
        let smoothing = SmootherConfig {
            smooth_seconds: 1.0,
            settle_ratio: 0.25,
        };
        let mut bank = GainBank::new([GainDb::DEFAULT].into_iter(), 2.0, smoothing);
        assert!(bank.set_kill(0, true));
        bank.tick();
        assert_eq!(linear(&bank, 0).to_bits(), 0.5_f32.to_bits());
        bank.tick();
        assert_eq!(linear(&bank, 0).to_bits(), 0);
        assert!(bank.set_kill(0, false));
        bank.tick();
        assert_eq!(linear(&bank, 0).to_bits(), 0.5_f32.to_bits());
        bank.tick();
        assert_eq!(linear(&bank, 0).to_bits(), 1.0_f32.to_bits());
    }
}
