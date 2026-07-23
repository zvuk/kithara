use num_traits::cast::AsPrimitive;

use super::{MAX_GAIN_DB, MIN_GAIN_DB};

struct Consts;

impl Consts {
    const DB_DIVISOR: f32 = 20.0;
    const LOG_FREQ_BASE: f32 = 10.0;
    const MS_PER_SEC: f32 = 1000.0;
    const SMOOTH_BLOCK_SIZE: usize = 32;
    const SMOOTH_CONVERGENCE_THRESHOLD: f32 = 0.0001;
    const SMOOTH_TIME_MS: f32 = 10.0;
}

struct GainState {
    current_linear: f32,
    target_db: f32,
    target_linear: f32,
}

impl GainState {
    fn new(gain_db: f32) -> Self {
        let linear = db_to_linear(gain_db);
        Self {
            target_db: gain_db,
            target_linear: linear,
            current_linear: linear,
        }
    }

    fn set_target(&mut self, gain_db: f32) {
        let clamped = gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        if (clamped - self.target_db).abs() < f32::EPSILON {
            return;
        }
        self.target_db = clamped;
        self.target_linear = db_to_linear(clamped);
    }

    #[inline]
    fn smooth(&mut self, coeff: f32) {
        let diff = self.target_linear - self.current_linear;
        if diff.abs() < Consts::SMOOTH_CONVERGENCE_THRESHOLD {
            self.current_linear = self.target_linear;
        } else {
            self.current_linear += coeff * diff;
        }
    }
}

pub(crate) struct GainBank {
    gains: Vec<GainState>,
    smooth_coeff: f32,
    block_counter: usize,
    bypass_active: bool,
    silence_active: bool,
}

impl GainBank {
    pub(crate) fn new(gains_db: impl Iterator<Item = f32>, sample_rate: f32) -> Self {
        let gains = gains_db.map(GainState::new).collect();
        let mut bank = Self {
            gains,
            smooth_coeff: compute_smooth_coeff(sample_rate),
            block_counter: 0,
            bypass_active: false,
            silence_active: false,
        };
        bank.refresh_fastpath();
        bank
    }

    pub(crate) fn len(&self) -> usize {
        self.gains.len()
    }

    pub(crate) fn linear(&self, band: usize) -> f32 {
        self.gains[band].current_linear
    }

    pub(crate) fn tick(&mut self) {
        self.block_counter += 1;
        if self.block_counter < Consts::SMOOTH_BLOCK_SIZE {
            return;
        }
        self.block_counter = 0;
        for gain in &mut self.gains {
            gain.smooth(self.smooth_coeff);
        }
        self.refresh_fastpath();
    }

    pub(crate) fn bypass_active(&self) -> bool {
        self.bypass_active
    }

    pub(crate) fn silence_active(&self) -> bool {
        self.silence_active
    }

    pub(crate) fn reset(&mut self) {
        for gain in &mut self.gains {
            gain.target_db = 0.0;
            gain.target_linear = 1.0;
            gain.current_linear = 1.0;
        }
        self.block_counter = 0;
        self.refresh_fastpath();
    }

    pub(crate) fn set(&mut self, band: usize, gain_db: f32) {
        if let Some(state) = self.gains.get_mut(band) {
            state.set_target(gain_db);
        }
        self.refresh_fastpath();
    }

    pub(crate) fn target(&self, band: usize) -> Option<f32> {
        self.gains.get(band).map(|state| state.target_db)
    }

    pub(crate) fn update_sample_rate(&mut self, sample_rate: f32) {
        self.smooth_coeff = compute_smooth_coeff(sample_rate);
    }

    #[cfg(test)]
    pub(crate) fn is_smoothing(&self) -> bool {
        self.gains.iter().any(|gain| {
            (gain.target_linear - gain.current_linear).abs() > Consts::SMOOTH_CONVERGENCE_THRESHOLD
        })
    }

    #[cfg(test)]
    pub(crate) fn force_current(&mut self, band: usize, linear: f32) {
        self.gains[band].current_linear = linear;
        self.refresh_fastpath();
    }

    fn refresh_fastpath(&mut self) {
        self.bypass_active = !self.gains.is_empty()
            && self.gains.iter().all(|gain| {
                (gain.target_linear - 1.0).abs() < f32::EPSILON
                    && (gain.current_linear - 1.0).abs() < f32::EPSILON
            });
        self.silence_active = !self.gains.is_empty()
            && self.gains.iter().all(|gain| {
                gain.target_linear.abs() < f32::EPSILON && gain.current_linear.abs() < f32::EPSILON
            });
    }
}

#[inline]
pub(crate) fn clamp_sample(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

#[inline]
fn db_to_linear(db: f32) -> f32 {
    if db <= MIN_GAIN_DB {
        0.0
    } else {
        Consts::LOG_FREQ_BASE.powf(db / Consts::DB_DIVISOR)
    }
}

fn compute_smooth_coeff(sample_rate: f32) -> f32 {
    let tau = Consts::SMOOTH_TIME_MS / Consts::MS_PER_SEC;
    let block_size_f32: f32 = Consts::SMOOTH_BLOCK_SIZE.as_();
    let effective_rate = sample_rate / block_size_f32;
    1.0 - (-1.0 / (tau * effective_rate)).exp()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn db_to_linear_kill_at_min() {
        assert!(db_to_linear(MIN_GAIN_DB).abs() < f32::EPSILON);
        assert!(db_to_linear(-30.0).abs() < f32::EPSILON);
    }

    #[kithara::test]
    #[case::unity_at_zero(0.0, 1.0, 0.001)]
    #[case::boost_at_6db(6.0, 2.0, 0.02)]
    fn db_to_linear_maps_to_gain(#[case] db: f32, #[case] expected: f32, #[case] eps: f32) {
        assert!((db_to_linear(db) - expected).abs() < eps);
    }
}
