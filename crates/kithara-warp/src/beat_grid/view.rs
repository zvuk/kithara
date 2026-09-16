use std::fmt::Debug;

use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridStamp,
    BeatGridState,
};
use crate::{Beat, BeatsPerMinute, MapAxis, MapPoint, MapPosition, Meter};

/// One immutable, revisioned view of musical timing facts.
///
/// Every observable answer, including `state` and `axis`, must remain stable
/// for the lifetime of the view. New or refined facts require a new revision
/// and a new view.
pub trait BeatGridView: Debug + Send + Sync + 'static {
    /// Returns the native coordinate axis used by this view.
    fn axis(&self) -> MapAxis;

    /// Resolves a stamped native position to a stamped beat.
    fn beat_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>>;

    /// Resolves the beat at a native position, or the next mapped beat after a gap.
    fn beat_at_or_next(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        self.beat_at(position)
    }

    /// Returns the stable identity of the owning live grid.
    fn id(&self) -> BeatGridId;

    /// Resolves meter at a stamped beat.
    fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;

    /// Resolves a stamped beat to a stamped native position.
    fn position_at(
        &self,
        beat: MapPoint<Beat>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>;

    /// Resolves the affine region containing a stamped native position.
    fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion>;

    /// Returns the immutable revision represented by this view.
    fn revision(&self) -> BeatGridRevision;

    /// Returns the composite identity and revision.
    fn stamp(&self) -> BeatGridStamp {
        BeatGridStamp::new(self.id(), self.revision())
    }

    /// Returns the lifecycle state represented by this view.
    fn state(&self) -> BeatGridState;

    /// Resolves local tempo at a stamped native position.
    fn tempo_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>>;
}
