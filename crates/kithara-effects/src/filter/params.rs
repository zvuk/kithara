use kithara_derive::Ranged;
use num_traits::ToPrimitive;

use super::FilterSettingsChange;
use crate::GainDb;

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
    pub fn change(self, value: f64) -> Option<FilterSettingsChange> {
        match self {
            Self::CutoffHz => Cutoff::checked(value).map(FilterSettingsChange::CutoffHz),
            Self::Q => Q::checked(value).map(FilterSettingsChange::Q),
            Self::GainDb => {
                if !(f64::from(f32::from(GainDb::MIN))..=f64::from(f32::from(GainDb::MAX)))
                    .contains(&value)
                {
                    return None;
                }
                GainDb::checked(value.to_f32()?).map(FilterSettingsChange::GainDb)
            }
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

impl From<FilterSettingsChange> for f64 {
    fn from(change: FilterSettingsChange) -> Self {
        match change {
            FilterSettingsChange::CutoffHz(value) => Self::from(value),
            FilterSettingsChange::Q(value) => Self::from(value),
            FilterSettingsChange::GainDb(value) => Self::from(f32::from(value)),
        }
    }
}
