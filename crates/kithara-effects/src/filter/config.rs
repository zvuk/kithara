use bon::bon;
use kithara_bufpool::PoolRegion;
use kithara_config::Config;
use kithara_dsp::filter::{Coefficients, FilterError, Hertz, Type};

use super::{Cutoff, Q};

/// The pass response the knob selects on one side of its center.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LowPass,
    HighPass,
}

/// The deck filter's fixed sweep ranges and resonance.
///
/// The low-pass cutoff falls from `low_edge` next to the center to
/// `low_extreme` at the left end; the high-pass cutoff rises from `high_edge`
/// to `high_extreme` at the right end. Both follow a geometric map of the
/// knob's distance from center.
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(builder(existing), fields(value, get(copy)))]
#[non_exhaustive]
pub struct FilterSettings {
    /// Low-pass cutoff next to the center.
    low_edge: Cutoff,
    /// Low-pass cutoff at the left end of the knob.
    low_extreme: Cutoff,
    /// High-pass cutoff next to the center.
    high_edge: Cutoff,
    /// High-pass cutoff at the right end of the knob.
    high_extreme: Cutoff,
    /// Resonance of both responses.
    q: Q,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            low_edge: Cutoff::LOW_EDGE,
            low_extreme: Cutoff::LOW_EXTREME,
            high_edge: Cutoff::HIGH_EDGE,
            high_extreme: Cutoff::HIGH_EXTREME,
            q: Q::default(),
        }
    }
}

#[bon]
impl FilterSettings {
    /// # Errors
    /// Refuses a low-pass range that does not fall away from center or a
    /// high-pass range that does not rise away from it.
    #[builder(
        builder_type(vis = "pub"),
        start_fn(name = builder, vis = "pub"),
        finish_fn(vis = "pub")
    )]
    fn new(
        #[builder(default = Cutoff::LOW_EDGE)] low_edge: Cutoff,
        #[builder(default = Cutoff::LOW_EXTREME)] low_extreme: Cutoff,
        #[builder(default = Cutoff::HIGH_EDGE)] high_edge: Cutoff,
        #[builder(default = Cutoff::HIGH_EXTREME)] high_extreme: Cutoff,
        #[builder(default)] q: Q,
    ) -> Result<Self, FilterError> {
        if low_extreme >= low_edge || high_extreme <= high_edge {
            return Err(FilterError::Parameters);
        }
        Ok(Self {
            low_edge,
            low_extreme,
            high_edge,
            high_extreme,
            q,
        })
    }

    /// The response a knob position asks for, `None` at the center.
    #[must_use]
    pub fn response(&self, knob: f64) -> Option<(Kind, f64)> {
        if knob < 0.0 {
            Some((Kind::LowPass, self.cutoff(Kind::LowPass, -knob)))
        } else if knob > 0.0 {
            Some((Kind::HighPass, self.cutoff(Kind::HighPass, knob)))
        } else {
            None
        }
    }

    /// Geometric interpolation from the edge (distance 0) to the extreme (distance 1).
    fn cutoff(&self, kind: Kind, distance: f64) -> f64 {
        let (edge, extreme) = match kind {
            Kind::LowPass => (self.low_edge, self.low_extreme),
            Kind::HighPass => (self.high_edge, self.high_extreme),
        };
        let edge = f64::from(edge);
        edge * (f64::from(extreme) / edge).powf(distance)
    }

    /// Coefficients for `knob` at `rate`, `None` at the center.
    pub(super) fn coefficients(
        &self,
        rate: f64,
        knob: f64,
    ) -> Result<Option<Coefficients<f64>>, FilterError> {
        let Some((kind, cutoff)) = self.response(knob) else {
            return Ok(None);
        };
        if !cutoff.is_finite() || cutoff >= rate / 2.0 {
            return Err(FilterError::Parameters);
        }
        let kind = match kind {
            Kind::LowPass => Type::LowPass,
            Kind::HighPass => Type::HighPass,
        };
        Coefficients::from_params(
            kind,
            Hertz::from_hz(rate).map_err(|_| FilterError::Parameters)?,
            Hertz::from_hz(cutoff).map_err(|_| FilterError::Parameters)?,
            f64::from(self.q),
        )
        .map(Some)
        .map_err(|_| FilterError::Parameters)
    }

    /// Cutoffs swept by knob positions in `low..=high`, `None` when only the center is.
    pub(super) fn cutoff_hull(&self, low: f64, high: f64) -> Option<[f64; 2]> {
        let mut hull: Option<[f64; 2]> = None;
        let mut include = |value: f64| {
            hull = Some(hull.map_or([value, value], |[min, max]| {
                [min.min(value), max.max(value)]
            }));
        };
        if low < 0.0 {
            include(self.cutoff(Kind::LowPass, -low));
            include(self.cutoff(Kind::LowPass, -high.min(0.0)));
        }
        if high > 0.0 {
            include(self.cutoff(Kind::HighPass, low.max(0.0)));
            include(self.cutoff(Kind::HighPass, high));
        }
        hull
    }

    /// Cutoff travel of a knob move in warped log Hz at `rate`.
    ///
    /// A move through the center travels to its edge on one side and from the
    /// other edge on the other; the response restarts there from reset state.
    /// # Errors
    /// Refuses nonfinite knob values or cutoffs at or above Nyquist.
    pub fn travel(&self, rate: f64, from: f64, to: f64) -> Result<f64, FilterError> {
        if !from.is_finite() || !to.is_finite() {
            return Err(FilterError::Parameters);
        }
        let side = |kind: Kind, from: f64, to: f64| -> Result<f64, FilterError> {
            let warp = |distance: f64| {
                let cutoff = self.cutoff(kind, distance);
                if cutoff >= rate / 2.0 {
                    Err(FilterError::Parameters)
                } else {
                    Ok((std::f64::consts::PI * cutoff / rate).tan().ln())
                }
            };
            Ok((warp(to)? - warp(from)?).abs())
        };
        let low = if from < 0.0 || to < 0.0 {
            side(Kind::LowPass, -from.min(0.0), -to.min(0.0))?
        } else {
            0.0
        };
        let high = if from > 0.0 || to > 0.0 {
            side(Kind::HighPass, from.max(0.0), to.max(0.0))?
        } else {
            0.0
        };
        let travel = low + high;
        if travel.is_finite() {
            Ok(travel)
        } else {
            Err(FilterError::Parameters)
        }
    }

    /// Stability margin of every pass response with a cutoff in `cutoff` at `rate`.
    pub(super) fn range_margin(&self, rate: f64, cutoff: [f64; 2]) -> Result<f64, FilterError> {
        let [low, high] = cutoff;
        if !low.is_finite() || !high.is_finite() || low <= 0.0 || low > high || high >= rate / 2.0 {
            return Err(FilterError::Parameters);
        }
        let q = f64::from(self.q);
        let angles = cutoff.map(|hz| std::f64::consts::TAU * hz / rate);
        let sines = angles.map(f64::sin);
        let minimum_sine = sines[0].min(sines[1]);
        let maximum_sine = if angles[0] <= std::f64::consts::FRAC_PI_2
            && angles[1] >= std::f64::consts::FRAC_PI_2
        {
            1.0
        } else {
            sines[0].max(sines[1])
        };
        let maximum_cosine = angles
            .map(|angle| angle.cos().abs())
            .into_iter()
            .fold(0.0, f64::max);
        let denominator_max = 2.0 + 2.0 * maximum_sine / (2.0 * q);
        let schur = 4.0 * (1.0 - maximum_cosine) / denominator_max;
        let damping = 4.0 * (minimum_sine / (2.0 * q)) / denominator_max;
        let lower_pole = 4.0 / denominator_max;
        // Reserve coefficient and comparison rounding across the continuous parameter box.
        let rounding = f64::from(f32::EPSILON);
        if schur > 1.5 * rounding && damping > 0.5 * rounding && lower_pole > 0.5 * rounding {
            Ok((schur - 1.5 * rounding)
                .min(damping - 0.5 * rounding)
                .min(lower_pole - 0.5 * rounding))
        } else {
            Err(FilterError::Parameters)
        }
    }
}

/// Settings and scratch resources of the deck filter.
#[derive(derive_more::From)]
#[derive_where::derive_where(Clone, Debug)]
#[non_exhaustive]
pub struct FilterConfig<S> {
    /// Scratch storage supplied by the owning playback region.
    pub(super) pools: PoolRegion<S>,
    /// Fixed sweep ranges and resonance.
    pub(super) settings: FilterSettings,
}
