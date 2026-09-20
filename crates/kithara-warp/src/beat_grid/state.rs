use super::BeatGridStamp;
use crate::{BeatEvidence, FrameUncertainty, MapRegion};

/// Why a grid cannot currently answer queries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BeatGridUnavailable {
    /// The supplied coordinate uses another grid-native axis.
    AxisMismatch,
    /// No usable geometry is available for this grid.
    NoGeometry,
    /// Beat geometry exists, but no meter evidence is available.
    NoMeter,
}

/// Lifecycle state of one immutable grid snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BeatGridState {
    /// Partial geometry may gain coverage in later revisions.
    Building,
    /// The bounded grid will not gain more coverage.
    Complete,
    /// The grid describes a live coordinate domain.
    Live,
    /// The grid cannot currently answer any query.
    Unavailable(BeatGridUnavailable),
}

/// Affine grid region containing one queried native position.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum BeatGridRegion {
    /// The current relation has no finite piecewise boundary.
    Unbounded,
    /// The current relation is valid inside this exact native region.
    Bounded(MapRegion),
}

/// A resolved value and the evidence supporting it.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct BeatEstimate<T> {
    /// Returns how the value was established.
    #[field(get, copy)]
    evidence: BeatEvidence,
    /// Returns the snapshot identity and revision used by the estimate.
    #[field(get, copy)]
    stamp: BeatGridStamp,
    /// Returns maximum absolute error in grid-native frames.
    #[field(get, copy)]
    uncertainty: FrameUncertainty,
    /// Returns the resolved value.
    #[field(get)]
    value: T,
}

impl<T> BeatEstimate<T> {
    /// Creates one resolved value with its provenance and grid stamp.
    #[must_use]
    pub const fn new(
        value: T,
        evidence: BeatEvidence,
        uncertainty: FrameUncertainty,
        stamp: BeatGridStamp,
    ) -> Self {
        Self {
            evidence,
            stamp,
            uncertainty,
            value,
        }
    }
}

/// Typed result of a beat-grid query.
#[derive(Clone, Debug, PartialEq)]
#[must_use]
#[non_exhaustive]
pub enum BeatGridQuery<T> {
    /// The snapshot resolved the requested coordinate.
    Resolved(T),
    /// A later revision may cover the requested grid region.
    Uncovered { required: MapRegion },
    /// The complete bounded grid proves the coordinate is outside its domain.
    OutsideDomain,
    /// The supplied point belongs to another grid identity or revision.
    Stale {
        expected: BeatGridStamp,
        given: BeatGridStamp,
    },
    /// The grid cannot answer the query for the stated reason.
    Unavailable(BeatGridUnavailable),
}

impl<T> BeatGridQuery<T> {
    /// Carries a resolved value into a dependent query, keeping every refusal.
    ///
    /// A projection answers by asking two grids in turn, and the caller must
    /// learn which of them refused and why, so a refusal travels out unchanged
    /// rather than collapsing into one opaque failure.
    pub(crate) fn and_then<U>(
        self,
        resolve: impl FnOnce(T) -> BeatGridQuery<U>,
    ) -> BeatGridQuery<U> {
        match self {
            Self::Resolved(value) => resolve(value),
            Self::Uncovered { required } => BeatGridQuery::Uncovered { required },
            Self::OutsideDomain => BeatGridQuery::OutsideDomain,
            Self::Stale { expected, given } => BeatGridQuery::Stale { expected, given },
            Self::Unavailable(reason) => BeatGridQuery::Unavailable(reason),
        }
    }
}
