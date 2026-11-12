use bon::bon;
use kithara_config::Config;
use kithara_dsp::filter::FilterError;

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

    /// Refuses a rate whose Nyquist frequency any configured cutoff reaches.
    pub(super) fn check(&self, rate: f64) -> Result<(), FilterError> {
        let nyquist = rate / 2.0;
        if [
            self.low_edge,
            self.low_extreme,
            self.high_edge,
            self.high_extreme,
        ]
        .into_iter()
        .all(|cutoff| f64::from(cutoff) < nyquist)
        {
            Ok(())
        } else {
            Err(FilterError::Parameters)
        }
    }
}
