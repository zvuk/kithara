use kithara_derive::Ranged;

use super::FilterSettingsChange;

/// Filter cutoff in Hz; preparation also requires it to be below Nyquist.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 20.0, max = 20000.0)]
pub struct Cutoff(f64);

/// Positive RBJ quality factor, defaulting to the Butterworth response.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ranged)]
#[ranged(min = 0.1, max = 10.0)]
pub struct Q(f64);

impl Default for Q {
    fn default() -> Self {
        Self(std::f64::consts::FRAC_1_SQRT_2)
    }
}

/// A live filter field, with its canonical units and admission range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterParameter {
    CutoffHz,
    Q,
    GainDb,
}

impl FilterParameter {
    pub const ALL: [Self; 3] = [Self::CutoffHz, Self::Q, Self::GainDb];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::CutoffHz => 0,
            Self::Q => 1,
            Self::GainDb => 2,
        }
    }

    #[must_use]
    pub const fn of(change: FilterSettingsChange) -> Self {
        match change {
            FilterSettingsChange::CutoffHz(_) => Self::CutoffHz,
            FilterSettingsChange::Q(_) => Self::Q,
            FilterSettingsChange::GainDb(_) => Self::GainDb,
        }
    }
}

impl FilterSettingsChange {
    #[must_use]
    pub fn value(self) -> f64 {
        match self {
            Self::CutoffHz(value) => f64::from(value),
            Self::Q(value) => f64::from(value),
            Self::GainDb(value) => f64::from(f32::from(value)),
        }
    }
}
