use bon::bon;
use kithara_bufpool::PoolRegion;
use kithara_config::Config;
use kithara_dsp::filter::{Coefficients, FilterError, Hertz, Type};

use super::{Cutoff, Q};
use crate::GainDb;

/// The four causal conditioning responses supported by [`FilterConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    HighPass,
    LowPass,
    LowShelf,
    HighShelf,
}

impl Kind {
    fn filter_type(self, gain: f64) -> Result<Type<f64>, FilterError> {
        match self {
            Self::HighPass if gain == 0.0 => Ok(Type::HighPass),
            Self::LowPass if gain == 0.0 => Ok(Type::LowPass),
            Self::LowShelf => Ok(Type::LowShelf(gain)),
            Self::HighShelf => Ok(Type::HighShelf(gain)),
            _ => Err(FilterError::Parameters),
        }
    }
}

/// Validated construction settings for one second-order filter.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(builder(existing), fields(value, get(copy)))]
#[non_exhaustive]
pub struct FilterSettings {
    /// Pass response or shelf polarity.
    kind: Kind,
    /// Frequency in Hz, below Nyquist at preparation time.
    #[config(live)]
    cutoff_hz: Cutoff,
    /// Shelf gain in `[-24, 6]` dB; pass filters require zero.
    #[config(live)]
    gain_db: GainDb,
    /// Resonance within the admitted positive range.
    #[config(live)]
    q: Q,
}

#[bon]
impl FilterSettings {
    #[builder(
        builder_type(vis = "pub"),
        start_fn(name = builder, vis = "pub"),
        finish_fn(vis = "pub")
    )]
    fn new(
        kind: Kind,
        cutoff_hz: Cutoff,
        #[builder(default)] q: Q,
        #[builder(default)] gain_db: GainDb,
    ) -> Result<Self, FilterError> {
        kind.filter_type(f64::from(f32::from(gain_db)))?;
        Ok(Self {
            kind,
            cutoff_hz,
            gain_db,
            q,
        })
    }

    pub(super) fn coefficients(&self, rate: f64) -> Result<Coefficients<f64>, FilterError> {
        self.coefficients_at(
            rate,
            f64::from(self.cutoff_hz),
            f64::from(self.q),
            f64::from(f32::from(self.gain_db)),
        )
    }

    pub(super) fn coefficients_at(
        &self,
        rate: f64,
        cutoff: f64,
        q: f64,
        gain: f64,
    ) -> Result<Coefficients<f64>, FilterError> {
        if cutoff >= rate / 2.0 {
            return Err(FilterError::Parameters);
        }
        let kind = self.kind.filter_type(gain)?;
        let design =
            || Coefficients::from_params(kind, Hertz::from_hz(rate)?, Hertz::from_hz(cutoff)?, q);
        design().map_err(|_| FilterError::Parameters)
    }
    pub(super) fn admit_smoothing(
        &self,
        rate: f64,
        cutoff: f64,
        q: f64,
        gain: f64,
    ) -> Result<(), FilterError> {
        let target_cutoff = f64::from(self.cutoff_hz);
        let target_q = f64::from(self.q);
        let target_gain = f64::from(f32::from(self.gain_db));
        if ![cutoff, q, gain].iter().all(|value| value.is_finite())
            || cutoff <= 0.0
            || q <= 0.0
            || cutoff.max(target_cutoff) >= rate / 2.0
        {
            return Err(FilterError::Parameters);
        }
        self.kind.filter_type(gain)?;
        self.kind.filter_type(target_gain)?;
        let angles = [cutoff.min(target_cutoff), cutoff.max(target_cutoff)]
            .map(|hz| std::f64::consts::TAU * hz / rate);
        let [low, high] = angles;
        let sines = angles.map(f64::sin);
        let minimum_sine = sines[0].min(sines[1]);
        let maximum_sine =
            if low <= std::f64::consts::FRAC_PI_2 && high >= std::f64::consts::FRAC_PI_2 {
                1.0
            } else {
                sines[0].max(sines[1])
            };
        let cosine = angles.map(|angle| angle.cos().abs());
        let maximum_cosine = cosine[0].max(cosine[1]);
        let alpha_min = minimum_sine / (2.0 * q.max(target_q));
        let alpha_max = maximum_sine / (2.0 * q.min(target_q));
        let gains = if matches!(self.kind, Kind::LowShelf | Kind::HighShelf) {
            [gain.min(target_gain), gain.max(target_gain)].map(|db| 10.0_f64.powf(db / 40.0))
        } else {
            [1.0, 1.0]
        };
        let [gain_min, gain_max] = gains;
        let denominator_max = gain_max
            + 1.0
            + (gain_min - 1.0).abs().max((gain_max - 1.0).abs()) * maximum_cosine
            + 2.0 * gain_max.sqrt() * alpha_max;
        let schur = 4.0 * gain_min.min(1.0) * (1.0 - maximum_cosine) / denominator_max;
        let damping = 4.0 * gain_min.sqrt() * alpha_min / denominator_max;
        let lower_pole = 4.0 * gain_min.min(1.0) / denominator_max;
        // Reserve coefficient and comparison rounding across the continuous parameter box.
        let rounding = f64::from(f32::EPSILON);
        if schur > 1.5 * rounding && damping > 0.5 * rounding && lower_pole > 0.5 * rounding {
            Ok(())
        } else {
            Err(FilterError::Parameters)
        }
    }
}

/// Parameters and scratch resources of one prepared second-order filter.
#[derive(Config, derive_more::From)]
#[derive_where::derive_where(Clone, Debug)]
#[config(builder(existing), fields(value, get(copy)))]
#[non_exhaustive]
pub struct FilterConfig<S> {
    /// Scratch storage supplied by the owning playback region.
    #[config(skip = "injected pool region", get(ref))]
    pub(super) pools: PoolRegion<S>,
    /// Fixed settings carried into this prepared instance.
    pub(super) settings: FilterSettings,
}

#[bon]
impl<S> FilterConfig<S> {
    #[builder(
        builder_type(vis = "pub"),
        start_fn(name = builder, vis = "pub"),
        finish_fn(vis = "pub")
    )]
    fn new(
        #[builder(start_fn)] pools: PoolRegion<S>,
        kind: Kind,
        cutoff_hz: Cutoff,
        #[builder(default)] q: Q,
        #[builder(default)] gain_db: GainDb,
    ) -> Result<Self, FilterError> {
        let settings = FilterSettings::builder()
            .kind(kind)
            .cutoff_hz(cutoff_hz)
            .q(q)
            .gain_db(gain_db)
            .build()?;
        Ok(Self::from((pools, settings)))
    }

    pub(super) fn coefficients(&self, rate: f64) -> Result<Coefficients<f64>, FilterError> {
        self.settings.coefficients(rate)
    }
}
