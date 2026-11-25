use kithara_command::{ScopeId, Seq};
use kithara_effects::GainDb;
use kithara_signal::SessionFrame;

use super::{ParameterCurve, ParameterCurveError, ParameterKnot};

/// Identity of the layout that gives an EQ band or FX unit address its meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutRevision {
    scope: ScopeId,
    instance: Option<Seq>,
}

impl LayoutRevision {
    #[must_use]
    pub fn scope(&self) -> ScopeId {
        self.scope
    }

    #[must_use]
    pub fn instance(&self) -> Option<Seq> {
        self.instance
    }

    pub(crate) const fn new(scope: ScopeId, instance: Option<Seq>) -> Self {
        Self { scope, instance }
    }
}

/// What an FX program moves in its unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FxTarget {
    /// The unit's dry/wet.
    Mix,
    /// A continuous parameter the held effect declares, by its index.
    Param(usize),
}

/// A continuous channel control, one band in its observed EQ layout, or a
/// target of one FX unit under its observed effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeckParameter {
    Trim,
    Volume,
    Crossfader,
    Filter,
    EqBand {
        band: usize,
        layout: LayoutRevision,
    },
    Fx {
        unit: usize,
        layout: LayoutRevision,
        target: FxTarget,
    },
}

impl DeckParameter {
    pub(crate) const CHANNELS: usize = 3;
    pub(crate) const TRIM: usize = 0;
    pub(crate) const VOLUME: usize = 1;
    pub(crate) const CROSSFADER: usize = 2;
    pub(crate) const FILTER: usize = Self::CHANNELS;
    pub(crate) const EQ_START: usize = Self::FILTER + 1;

    pub(crate) const fn eq_index(band: usize) -> usize {
        Self::EQ_START.saturating_add(band)
    }

    /// The index of a control every deck has, whatever its layouts.
    pub(crate) const fn fixed(self) -> Option<usize> {
        match self {
            Self::Trim => Some(Self::TRIM),
            Self::Volume => Some(Self::VOLUME),
            Self::Crossfader => Some(Self::CROSSFADER),
            Self::Filter => Some(Self::FILTER),
            Self::EqBand { .. } | Self::Fx { .. } => None,
        }
    }
}

/// Prepared native-unit values for one deck parameter on session frames.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct DeckProgram {
    parameter: DeckParameter,
    #[field(#[must_use] get, vis = "pub(crate)")]
    curve: ParameterCurve,
}

impl DeckProgram {
    #[must_use]
    pub fn parameter(&self) -> DeckParameter {
        self.parameter
    }

    pub const MAX_KNOTS: usize = ParameterCurve::MAX_KNOTS;

    /// Checks ordered knots and the addressed control's native range off the
    /// audio thread; an effect parameter's range is its descriptor's, checked
    /// by the deck against the effect it holds.
    /// # Errors
    /// Refuses malformed knots and nonfinite or out-of-range values.
    pub fn new(
        parameter: DeckParameter,
        knots: Vec<ParameterKnot>,
    ) -> Result<Self, ParameterCurveError> {
        let curve = ParameterCurve::new(knots)?;
        let range = curve.value_range();
        let domain = match parameter {
            DeckParameter::Filter => -1.0..=1.0,
            DeckParameter::EqBand { .. } => {
                f64::from(f32::from(GainDb::MIN))..=f64::from(f32::from(GainDb::MAX))
            }
            // The held effect's descriptor bounds the knots when the deck takes the program.
            DeckParameter::Fx {
                target: FxTarget::Param(_),
                ..
            } => f64::MIN..=f64::MAX,
            _ => 0.0..=1.0,
        };
        if !domain.contains(range.start()) || !domain.contains(range.end()) {
            return Err(ParameterCurveError::Range);
        }
        Ok(Self { parameter, curve })
    }

    delegate::delegate! {
        to self.curve {
            /// First addressed session frame.
            #[must_use]
            pub fn start(&self) -> SessionFrame;
            /// Last knot, whose value is held afterward.
            #[must_use]
            pub fn end(&self) -> SessionFrame;
        }
    }
}
