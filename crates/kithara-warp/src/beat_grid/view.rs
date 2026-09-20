use std::fmt::Debug;

use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridStamp,
    BeatGridState,
};
use crate::{AssetFrame, Beat, BeatsPerMinute, MapAxis, MapPoint, MapPosition, Meter};

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

    /// Resolves the tempo ratio this view applies to its source geometry.
    ///
    /// A view that describes a recording as analysed answers `1.0`; a
    /// projection answers the ratio that carries the source onto its target
    /// axis.
    fn rate_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<f64>;

    /// Resolves the recording frame that sounds at a stamped native position.
    ///
    /// A view that describes a recording as analysed answers the position it
    /// was asked about; a projection answers the frame of the recording it
    /// carries, which is the absolute relation a renderer reads.
    fn source_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<AssetFrame>;

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

/// Refuses a query stamped for a different revision of the same grid.
///
/// Every view answers only for the exact stamp it carries, so a caller holding
/// an older coordinate learns which revision it must re-resolve against
/// instead of silently reading numbers from the wrong geometry.
pub(super) fn stale<T>(grid: &impl BeatGridView, given: BeatGridStamp) -> Option<BeatGridQuery<T>> {
    let expected = grid.stamp();
    (given != expected).then_some(BeatGridQuery::Stale { expected, given })
}
