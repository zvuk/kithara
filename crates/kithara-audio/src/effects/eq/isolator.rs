use num_traits::cast::AsPrimitive;

use super::{
    EqBandConfig,
    filter::CrossoverFilters,
    gain::{GainBank, clamp_sample},
};

/// Single-channel isolator crossover EQ.
pub struct IsolatorEq {
    filters: CrossoverFilters,
    gains: GainBank,
    was_in_fastpath: bool,
}

impl IsolatorEq {
    #[must_use]
    pub fn new(bands: &[EqBandConfig], sample_rate: u32) -> Self {
        let sample_rate: f32 = sample_rate.as_();
        let crossover_freqs = bands
            .windows(2)
            .map(|pair| (pair[0].frequency() * pair[1].frequency()).sqrt())
            .collect();
        Self {
            filters: CrossoverFilters::new(crossover_freqs, bands.len(), sample_rate),
            gains: GainBank::new(bands.iter().map(EqBandConfig::gain_db), sample_rate),
            was_in_fastpath: false,
        }
    }

    #[must_use]
    pub fn band_count(&self) -> usize {
        self.gains.len()
    }

    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        self.gains.tick();
        if self.gains.silence_active() {
            self.filters.record(input);
            self.was_in_fastpath = true;
            return 0.0;
        }
        if self.gains.bypass_active() {
            self.filters.record(input);
            self.was_in_fastpath = true;
            return input;
        }
        if self.was_in_fastpath {
            self.was_in_fastpath = false;
            self.filters.rehydrate();
        }
        match self.gains.len() {
            0 => input,
            1 => clamp_sample(input * self.gains.linear(0)),
            _ => clamp_sample(self.filters.process(input, |band| self.gains.linear(band))),
        }
    }

    pub fn reset(&mut self) {
        self.gains.reset();
        self.filters.reset();
        self.was_in_fastpath = false;
    }

    pub fn set_gain(&mut self, band: usize, gain_db: f32) {
        self.gains.set(band, gain_db);
    }

    #[must_use]
    pub fn target_gain(&self, band: usize) -> Option<f32> {
        self.gains.target(band)
    }

    pub fn update_sample_rate(&mut self, sample_rate: u32) {
        let sample_rate = sample_rate.as_();
        self.gains.update_sample_rate(sample_rate);
        self.filters.update_sample_rate(sample_rate);
    }

    #[cfg(test)]
    pub(crate) fn is_smoothing(&self) -> bool {
        self.gains.is_smoothing()
    }
    #[cfg(test)]
    pub(crate) fn bypass_active(&self) -> bool {
        self.gains.bypass_active()
    }
    #[cfg(test)]
    pub(crate) fn silence_active(&self) -> bool {
        self.gains.silence_active()
    }
    #[cfg(test)]
    pub(crate) fn force_current_gain(&mut self, band: usize, linear: f32) {
        self.gains.force_current(band, linear);
    }
}
