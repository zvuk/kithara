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

    #[cfg(test)]
    fn is_smoothing(&self, smoothing: SmootherConfig) -> bool {
        !self.filter.has_settled(self.target_linear)
            && (self.filter.z1 - self.target_linear).abs()
                >= DEFAULT_GAIN_SPAN * smoothing.settle_ratio
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

    #[cfg(test)]
    pub(crate) fn is_smoothing(&self) -> bool {
        self.gains
            .iter()
            .any(|gain| gain.is_smoothing(self.smoothing))
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

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

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

        assert_eq!(bank.linear(0), GainDb::MAX.linear());
    }
}
