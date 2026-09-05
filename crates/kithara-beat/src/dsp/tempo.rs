//! The tempo the periodicity stage searches.

use std::ops::RangeInclusive;

use num_traits::cast::ToPrimitive;
use thiserror::Error;

use super::{
    consts::{PeriodConsts, TempoConsts},
    frames,
};

/// A tempo policy the periodicity stage cannot search.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum TempoError {
    /// The band is not an ascending range of finite positive tempi.
    #[error("tempo band {low}..={high} BPM is not an ascending range of finite positive tempi")]
    Band {
        /// Slowest tempo of the rejected band, in BPM.
        low: f32,
        /// Fastest tempo of the rejected band, in BPM.
        high: f32,
    },
    /// The prior is not a finite tempo inside the band.
    #[error("tempo prior {prior} BPM lies outside the band {low}..={high} BPM")]
    Prior {
        /// The rejected prior, in BPM.
        prior: f32,
        /// Slowest tempo of the band, in BPM.
        low: f32,
        /// Fastest tempo of the band, in BPM.
        high: f32,
    },
    /// The band covers hypotheses the comb leaves unscored.
    #[error("tempo band {low}..={high} BPM covers hypotheses {lags:?}, past the scored {scored:?}")]
    Unscored {
        /// Slowest tempo of the rejected band, in BPM.
        low: f32,
        /// Fastest tempo of the rejected band, in BPM.
        high: f32,
        /// Hypotheses the band covers.
        lags: RangeInclusive<usize>,
        /// Hypotheses the comb scores.
        scored: RangeInclusive<usize>,
    },
}

/// The tempo the signal detector searches: the band its period estimates stay
/// inside, and the tempo it prefers within that band, both in BPM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    low: f32,
    high: f32,
    prior: f32,
}

impl Tempo {
    /// The band, in BPM.
    #[must_use]
    pub fn band(&self) -> RangeInclusive<f32> {
        self.low..=self.high
    }

    /// The hypotheses the band covers.
    pub(super) fn lags(&self) -> RangeInclusive<usize> {
        hypothesis(self.high)..=hypothesis(self.low)
    }

    /// # Errors
    /// [`TempoError`] when the band or the prior falls where the periodicity
    /// stage reads nothing.
    pub fn new(band: RangeInclusive<f32>, prior: f32) -> Result<Self, TempoError> {
        let (low, high) = (*band.start(), *band.end());
        if !low.is_finite() || !high.is_finite() || low <= 0.0 || high < low {
            return Err(TempoError::Band { low, high });
        }
        if !prior.is_finite() || prior < low || prior > high {
            return Err(TempoError::Prior { prior, low, high });
        }
        let tempo = Self { low, high, prior };
        let lags = tempo.lags();
        let scored = PeriodConsts::PERIOD_INDEX;
        if !scored.contains(lags.start()) || !scored.contains(lags.end()) {
            return Err(TempoError::Unscored {
                low,
                high,
                lags,
                scored,
            });
        }
        Ok(tempo)
    }

    /// The preferred tempo, in BPM.
    #[must_use]
    pub const fn prior(&self) -> f32 {
        self.prior
    }

    /// The Rayleigh mode, in whole lags.
    pub(super) fn prior_lag(&self) -> f32 {
        lag(self.prior).round()
    }

    /// Where the salience the tracker reads begins: the band's shortest lag,
    /// less the comb's widest reach below a harmonic.
    pub(super) fn search_floor(&self) -> usize {
        self.lags()
            .start()
            .saturating_sub(PeriodConsts::COMB_HARMONICS - 1)
    }
}

impl Default for Tempo {
    fn default() -> Self {
        Self {
            low: TempoConsts::BAND_LOW_BPM,
            high: TempoConsts::BAND_HIGH_BPM,
            prior: TempoConsts::PRIOR_BPM,
        }
    }
}

fn hypothesis(beats_per_minute: f32) -> usize {
    lag(beats_per_minute)
        .round()
        .to_usize()
        .unwrap_or(0)
        .saturating_sub(1)
}

fn lag(beats_per_minute: f32) -> f32 {
    60.0 / (beats_per_minute * frames::frame_seconds())
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    /// The lags the default policy reads are the grid the goldens were
    /// recorded on.
    #[kithara::test(native, flash(false))]
    fn the_default_policy_reads_its_lags() {
        let tempo = Tempo::default();
        assert_eq!(tempo.lags(), 27..=107, "the band covers lags 28..=108");
        assert!(
            (tempo.prior_lag() - 43.0).abs() < f32::EPSILON,
            "the Rayleigh mode is 43 lags, read as {}",
            tempo.prior_lag()
        );
        assert_eq!(
            tempo.search_floor(),
            24,
            "the salience the tracker reads starts at lag 25"
        );
    }

    #[kithara::test(native, flash(false))]
    fn the_default_policy_is_one_new_accepts() {
        let tempo = Tempo::default();
        assert_eq!(Tempo::new(tempo.band(), tempo.prior()), Ok(tempo));
    }

    #[kithara::test(native, flash(false))]
    fn a_band_that_is_not_an_ascending_range_of_tempi_is_rejected() {
        for (low, high) in [
            (185.0, 48.0),
            (0.0, 185.0),
            (-48.0, 185.0),
            (f32::NAN, 185.0),
            (48.0, f32::INFINITY),
        ] {
            assert!(
                matches!(Tempo::new(low..=high, 120.0), Err(TempoError::Band { .. })),
                "{low}..={high} BPM was accepted as a band"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_prior_outside_the_band_is_rejected() {
        for prior in [47.0, 186.0, f32::NAN] {
            assert!(
                matches!(
                    Tempo::new(48.0..=185.0, prior),
                    Err(TempoError::Prior { .. })
                ),
                "{prior} BPM was accepted as a prior outside 48..=185 BPM"
            );
        }
    }

    /// The comb scores a fixed range of hypotheses, and a band reaching past
    /// it reads salience nothing wrote.
    #[kithara::test(native, flash(false))]
    fn a_band_the_comb_does_not_score_is_rejected() {
        for (low, high) in [(30.0, 185.0), (48.0, 2_000.0)] {
            assert!(
                matches!(
                    Tempo::new(low..=high, 120.0),
                    Err(TempoError::Unscored { .. })
                ),
                "{low}..={high} BPM was accepted as a searchable band"
            );
        }
    }
}
