//! The tempo the periodicity stage searches.

use std::ops::RangeInclusive;

use bon::bon;
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
    /// The tolerance is not a finite positive duration.
    #[error("beat tolerance {tolerance} s is not a finite positive duration")]
    Tolerance {
        /// The rejected tolerance, in seconds.
        tolerance: f32,
    },
    /// The drift is not a finite positive rate.
    #[error("tempo drift {drift} BPM per second is not a finite positive rate")]
    Drift {
        /// The rejected drift, in BPM per second.
        drift: f32,
    },
}

/// The tempo the signal detector searches: the band its period estimates stay
/// inside and the tempo it prefers within that band, both in BPM, together
/// with how tightly it holds a tempo once found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    low: f32,
    high: f32,
    prior: f32,
    tolerance: f32,
    drift: f32,
}

#[bon]
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

    /// A policy over the defaults: band and prior in BPM, tolerance in
    /// seconds, drift in BPM per second measured at the prior, so one rate
    /// reaches further in period the slower that prior is.
    ///
    /// # Errors
    /// [`TempoError`] when a value falls where the periodicity stage reads
    /// nothing, or is not the finite positive quantity it stands for.
    #[builder]
    pub fn new(
        band: Option<RangeInclusive<f32>>,
        prior: Option<f32>,
        tolerance: Option<f32>,
        drift: Option<f32>,
    ) -> Result<Self, TempoError> {
        let base = Self::default();
        let band = band.unwrap_or_else(|| base.band());
        let (low, high) = (*band.start(), *band.end());
        if !low.is_finite() || !high.is_finite() || low <= 0.0 || high < low {
            return Err(TempoError::Band { low, high });
        }
        let prior = prior.unwrap_or(base.prior);
        if !prior.is_finite() || prior < low || prior > high {
            return Err(TempoError::Prior { prior, low, high });
        }
        let tolerance = tolerance.unwrap_or(base.tolerance);
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(TempoError::Tolerance { tolerance });
        }
        let drift = drift.unwrap_or(base.drift);
        if !drift.is_finite() || drift <= 0.0 {
            return Err(TempoError::Drift { drift });
        }
        let tempo = Self {
            low,
            high,
            prior,
            tolerance,
            drift,
        };
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

    /// How far consecutive beats may fall from the period, in seconds.
    #[must_use]
    pub const fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// The tolerance in detection frames.
    pub(super) fn tolerance_frames(&self) -> f32 {
        self.tolerance / frames::frame_seconds()
    }

    /// The drift, as the between-estimate spread of the period, in lags.
    pub(super) fn transition_sigma(&self) -> f32 {
        self.drift * lags_per_drift(self.prior)
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
            tolerance: TempoConsts::TOLERANCE_SECONDS,
            drift: PeriodConsts::TRANSITION_SIGMA / lags_per_drift(TempoConsts::PRIOR_BPM),
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

/// Lags of period change per BPM per second of tempo change: the estimate
/// spacing in seconds, times the period's sensitivity to tempo at the prior.
fn lags_per_drift(prior: f32) -> f32 {
    let step_seconds = PeriodConsts::ACF_STEP.to_f32().unwrap_or(1.0) * frames::frame_seconds();
    step_seconds * lag(prior).round() / prior
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(native, flash(false))]
    fn the_default_policy_reads_the_grid_the_goldens_were_recorded_on() {
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
    fn the_default_policy_is_the_one_an_empty_builder_yields() {
        assert_eq!(Tempo::builder().build(), Ok(Tempo::default()));
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
                matches!(
                    Tempo::builder().band(low..=high).prior(120.0).build(),
                    Err(TempoError::Band { .. })
                ),
                "{low}..={high} BPM was accepted as a band"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_prior_outside_the_band_is_rejected() {
        for prior in [47.0, 186.0, f32::NAN] {
            assert!(
                matches!(
                    Tempo::builder().band(48.0..=185.0).prior(prior).build(),
                    Err(TempoError::Prior { .. })
                ),
                "{prior} BPM was accepted as a prior outside 48..=185 BPM"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_band_the_comb_does_not_score_is_rejected() {
        for (low, high) in [(30.0, 185.0), (48.0, 2_000.0)] {
            assert!(
                matches!(
                    Tempo::builder().band(low..=high).prior(120.0).build(),
                    Err(TempoError::Unscored { .. })
                ),
                "{low}..={high} BPM was accepted as a band, past the \
                 hypotheses the comb scores"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn the_default_policy_holds_the_calibrated_spread() {
        let tempo = Tempo::default();
        assert_eq!(
            tempo.transition_sigma(),
            PeriodConsts::TRANSITION_SIGMA,
            "the default drift is the calibrated lag spread, read as {} lags",
            tempo.transition_sigma()
        );
        assert_eq!(
            tempo.tolerance(),
            TempoConsts::TOLERANCE_SECONDS,
            "the default tolerance is the calibrated one"
        );
    }

    #[kithara::test(native, flash(false))]
    fn a_tolerance_that_is_not_a_finite_positive_duration_is_rejected() {
        for seconds in [0.0, -0.025, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    Tempo::builder().tolerance(seconds).build(),
                    Err(TempoError::Tolerance { .. })
                ),
                "{seconds} s was accepted as a beat tolerance"
            );
        }
    }

    #[kithara::test(native, flash(false))]
    fn a_drift_that_is_not_a_finite_positive_rate_is_rejected() {
        for rate in [0.0, -15.0, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    Tempo::builder().drift(rate).build(),
                    Err(TempoError::Drift { .. })
                ),
                "{rate} BPM per second was accepted as a tempo drift"
            );
        }
    }
}
