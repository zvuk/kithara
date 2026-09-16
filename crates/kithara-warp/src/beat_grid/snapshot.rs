use kithara_platform::sync::Arc;

use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridStamp,
    BeatGridState, BeatGridUnavailable, BeatGridView, segment::SegmentGridView,
    session::SessionGridView,
};
use crate::{
    Beat, BeatsPerMinute, MapAxis, MapPoint, MapPosition, Meter, MeterFacts, SegmentSet,
    SessionAnchor, SessionEpoch,
};

/// One immutable, revisioned beat-grid observation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BeatGridSnapshot {
    view: Arc<dyn BeatGridView>,
}

/// A concrete built-in beat-grid view is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BeatGridSnapshotError {
    /// Segment-backed grids are defined only in the bounded asset domain.
    #[error("segment-backed grid requires an asset axis, got {axis:?}")]
    InvalidAxis { axis: MapAxis },
    /// The lifecycle state is incompatible with the beat-grid coordinate axis.
    #[error("state {state:?} is invalid for beat-grid axis {axis:?}")]
    InvalidState { axis: MapAxis, state: BeatGridState },
}

impl BeatGridSnapshot {
    /// Validates and freezes an externally implemented immutable grid view.
    ///
    /// # Errors
    ///
    /// Returns [`BeatGridSnapshotError`] when the view's lifecycle state is
    /// incompatible with its native coordinate axis.
    pub fn freeze<V>(view: V) -> Result<Self, BeatGridSnapshotError>
    where
        V: BeatGridView,
    {
        Self::validate(view.axis(), view.state())?;
        Ok(Self::wrap(view))
    }

    /// Freezes validated segment-backed timing facts.
    ///
    /// # Errors
    ///
    /// Returns [`BeatGridSnapshotError`] when `state` is incompatible with the
    /// coordinate axis carried by `segments`.
    pub fn segments(
        id: BeatGridId,
        revision: BeatGridRevision,
        state: BeatGridState,
        segments: SegmentSet,
    ) -> Result<Self, BeatGridSnapshotError> {
        SegmentGridView::new(id, revision, state, segments).and_then(Self::freeze)
    }

    /// Freezes an ephemeral mathematical session grid.
    #[must_use]
    pub fn session(
        id: BeatGridId,
        revision: BeatGridRevision,
        epoch: SessionEpoch,
        anchor: SessionAnchor,
        meter: Option<MeterFacts>,
    ) -> Self {
        Self::wrap(SessionGridView::new(id, revision, epoch, anchor, meter))
    }

    /// Freezes a grid revision without usable geometry.
    #[must_use]
    pub fn unavailable(id: BeatGridId, revision: BeatGridRevision, axis: MapAxis) -> Self {
        Self::wrap(UnavailableGridView { id, revision, axis })
    }

    fn validate(axis: MapAxis, state: BeatGridState) -> Result<(), BeatGridSnapshotError> {
        let valid = matches!(
            (axis, state),
            (
                MapAxis::Asset(_),
                BeatGridState::Building | BeatGridState::Complete | BeatGridState::Unavailable(_)
            ) | (
                MapAxis::Session(_),
                BeatGridState::Live | BeatGridState::Unavailable(_)
            )
        );
        if valid {
            Ok(())
        } else {
            Err(BeatGridSnapshotError::InvalidState { axis, state })
        }
    }

    fn wrap<V>(view: V) -> Self
    where
        V: BeatGridView,
    {
        Self {
            view: Arc::new(view),
        }
    }

    delegate::delegate! {
        to self.view {
            /// Returns the stable grid identity.
            #[must_use]
            pub fn id(&self) -> BeatGridId;
            /// Returns the immutable grid revision.
            #[must_use]
            pub fn revision(&self) -> BeatGridRevision;
            /// Returns the snapshot lifecycle state.
            #[must_use]
            pub fn state(&self) -> BeatGridState;
            /// Returns the native coordinate axis used by this snapshot.
            #[must_use]
            pub fn axis(&self) -> MapAxis;
            /// Returns the composite identity and revision.
            #[must_use]
            pub fn stamp(&self) -> BeatGridStamp;
            /// Resolves the affine region containing a stamped native position.
            pub fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion>;
            /// Resolves a stamped native position to a stamped beat.
            pub fn beat_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
            /// Resolves the beat at a position, or the next mapped beat after a gap.
            pub fn beat_at_or_next(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
            /// Resolves a stamped beat to a stamped native position.
            pub fn position_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>;
            /// Resolves local tempo at a stamped native position.
            pub fn tempo_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>>;
            /// Resolves meter at a stamped beat.
            pub fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;
        }
    }
}

impl BeatGridView for BeatGridSnapshot {
    delegate::delegate! {
        to self.view {
            fn id(&self) -> BeatGridId;
            fn revision(&self) -> BeatGridRevision;
            fn state(&self) -> BeatGridState;
            fn axis(&self) -> MapAxis;
            fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion>;
            fn beat_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
            fn beat_at_or_next(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
            fn position_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>;
            fn tempo_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>>;
            fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;
        }
    }
}

impl PartialEq for BeatGridSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.stamp() == other.stamp()
    }
}

#[derive(Debug, fieldwork::Fieldwork)]
#[fieldwork(get, copy)]
struct UnavailableGridView {
    id: BeatGridId,
    revision: BeatGridRevision,
    axis: MapAxis,
}

impl UnavailableGridView {
    fn unavailable<T>(&self, given: BeatGridStamp) -> BeatGridQuery<T> {
        let expected = self.stamp();
        if given == expected {
            BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry)
        } else {
            BeatGridQuery::Stale { expected, given }
        }
    }

    fn unavailable_beat<T>(&self, beat: MapPoint<Beat>) -> BeatGridQuery<T> {
        self.unavailable(beat.stamp())
    }

    fn unavailable_position<T>(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<T> {
        let expected = self.stamp();
        let given = position.stamp();
        if given != expected {
            return BeatGridQuery::Stale { expected, given };
        }
        if position.value().kind() != self.axis.kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry)
    }
}

impl BeatGridView for UnavailableGridView {
    fn state(&self) -> BeatGridState {
        BeatGridState::Unavailable(BeatGridUnavailable::NoGeometry)
    }

    delegate::delegate! {
        to self {
            #[expr(*$)]
            fn id(&self) -> BeatGridId;
            #[expr(*$)]
            fn revision(&self) -> BeatGridRevision;
            #[expr(*$)]
            fn axis(&self) -> MapAxis;
            #[call(unavailable_position)]
            fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion>;
            #[call(unavailable_position)]
            fn beat_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
            #[call(unavailable_beat)]
            fn position_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>;
            #[call(unavailable_position)]
            fn tempo_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>>;
            #[call(unavailable_beat)]
            fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{AssetAxis, SessionFrame};

    #[derive(Debug)]
    struct StateOverride {
        snapshot: BeatGridSnapshot,
        state: BeatGridState,
    }

    impl BeatGridView for StateOverride {
        fn state(&self) -> BeatGridState {
            self.state
        }

        delegate::delegate! {
            to self.snapshot {
                fn id(&self) -> BeatGridId;
                fn revision(&self) -> BeatGridRevision;
                fn axis(&self) -> MapAxis;
                fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion>;
                fn beat_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;
                fn position_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>;
                fn tempo_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>>;
                fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;
            }
        }
    }

    #[kithara::test]
    fn freeze_rejects_an_invalid_external_axis_lifecycle_pair() {
        let sample_rate =
            NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero");
        let axis = MapAxis::Asset(AssetAxis::new(sample_rate, 1));
        let snapshot = BeatGridSnapshot::unavailable(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            axis,
        );
        let invalid = StateOverride {
            snapshot,
            state: BeatGridState::Live,
        };

        assert_eq!(
            BeatGridSnapshot::freeze(invalid),
            Err(BeatGridSnapshotError::InvalidState {
                axis,
                state: BeatGridState::Live,
            })
        );
    }

    #[kithara::test]
    fn unavailable_grid_preserves_native_axis_validation() {
        let sample_rate =
            NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero");
        let grid = BeatGridSnapshot::unavailable(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            MapAxis::Asset(AssetAxis::new(sample_rate, 1)),
        );
        let position = MapPoint::new(grid.stamp(), MapPosition::Session(SessionFrame::new(0)));

        assert_eq!(
            grid.region_at(position),
            BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch)
        );
    }
}
