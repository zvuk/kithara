use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision,
    BeatGridSnapshotError, BeatGridState, BeatGridUnavailable, BeatGridView, view::stale,
};
use crate::{
    AssetFrame, Beat, BeatsPerMinute, MapAxis, MapPoint, MapPosition, MapRegion, Meter, SegmentSet,
};

/// Immutable query view over validated sparse timing segments.
#[derive(Debug)]
pub(super) struct SegmentGridView {
    id: BeatGridId,
    revision: BeatGridRevision,
    state: BeatGridState,
    segments: SegmentSet,
}

impl SegmentGridView {
    pub(super) fn new(
        id: BeatGridId,
        revision: BeatGridRevision,
        state: BeatGridState,
        segments: SegmentSet,
    ) -> Result<Self, BeatGridSnapshotError> {
        let axis = segments.axis();
        if !matches!(axis, MapAxis::Asset(_)) {
            return Err(BeatGridSnapshotError::InvalidAxis { axis });
        }
        if matches!(state, BeatGridState::Live | BeatGridState::Unavailable(_)) {
            return Err(BeatGridSnapshotError::InvalidState { axis, state });
        }
        Ok(Self {
            id,
            revision,
            state,
            segments,
        })
    }

    fn missing_beat<T>(&self, beat: Beat) -> BeatGridQuery<T> {
        match self.state {
            BeatGridState::Complete => BeatGridQuery::OutsideDomain,
            BeatGridState::Building | BeatGridState::Live => BeatGridQuery::Uncovered {
                required: self.segments.uncovered_region_by_beat(beat),
            },
            BeatGridState::Unavailable(reason) => BeatGridQuery::Unavailable(reason),
        }
    }

    fn missing_meter<T>(&self, required: MapRegion) -> BeatGridQuery<T> {
        match self.state {
            BeatGridState::Building => BeatGridQuery::Uncovered { required },
            BeatGridState::Complete | BeatGridState::Live => {
                BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter)
            }
            BeatGridState::Unavailable(reason) => BeatGridQuery::Unavailable(reason),
        }
    }

    fn missing_position<T>(&self, position: MapPosition) -> BeatGridQuery<T> {
        match self.state {
            BeatGridState::Complete => BeatGridQuery::OutsideDomain,
            BeatGridState::Building | BeatGridState::Live => BeatGridQuery::Uncovered {
                required: self.segments.uncovered_region(position),
            },
            BeatGridState::Unavailable(reason) => BeatGridQuery::Unavailable(reason),
        }
    }

    fn outside_asset_extent(&self, position: MapPosition) -> bool {
        match (self.axis(), position) {
            (MapAxis::Asset(axis), MapPosition::Asset(frame)) => !axis.contains_or_eof(frame),
            _ => false,
        }
    }
}

impl BeatGridView for SegmentGridView {
    fn axis(&self) -> MapAxis {
        self.segments.axis()
    }

    fn beat_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        if let Some(stale) = stale(self, position.stamp()) {
            return stale;
        }
        if position.value().kind() != self.axis().kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        if self.outside_asset_extent(*position.value()) {
            return BeatGridQuery::OutsideDomain;
        }
        let Some((beat, evidence, uncertainty)) = self
            .segments
            .by_position(*position.value())
            .and_then(|segment| segment.beat_at(*position.value()))
        else {
            return self.missing_position(*position.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), beat),
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }

    fn beat_at_or_next(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        if let Some(stale) = stale(self, position.stamp()) {
            return stale;
        }
        if position.value().kind() != self.axis().kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        if self.outside_asset_extent(*position.value()) {
            return BeatGridQuery::OutsideDomain;
        }
        let Some((beat, evidence, uncertainty)) = self.segments.beat_at_or_next(*position.value())
        else {
            return self.missing_position(*position.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), beat),
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }

    fn id(&self) -> BeatGridId {
        self.id
    }

    fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>> {
        if let Some(stale) = stale(self, beat.stamp()) {
            return stale;
        }
        let Some(segment) = self.segments.by_beat(*beat.value()) else {
            return self.missing_beat(*beat.value());
        };
        let Some((meter, evidence, uncertainty)) = segment.meter_at(*beat.value()) else {
            return self.missing_meter(segment.region());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            meter,
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }

    fn position_at(
        &self,
        beat: MapPoint<Beat>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>> {
        if let Some(stale) = stale(self, beat.stamp()) {
            return stale;
        }
        let Some((position, evidence, uncertainty)) = self
            .segments
            .by_beat(*beat.value())
            .and_then(|segment| segment.position_at(*beat.value()))
        else {
            return self.missing_beat(*beat.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), position),
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }

    fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion> {
        if let Some(stale) = stale(self, position.stamp()) {
            return stale;
        }
        if position.value().kind() != self.axis().kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        if self.outside_asset_extent(*position.value()) {
            return BeatGridQuery::OutsideDomain;
        }
        self.segments.by_position(*position.value()).map_or_else(
            || self.missing_position(*position.value()),
            |segment| BeatGridQuery::Resolved(BeatGridRegion::Bounded(segment.region())),
        )
    }

    fn rate_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<f64> {
        self.source_at(position)
            .and_then(|_| BeatGridQuery::Resolved(1.0))
    }

    fn source_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<AssetFrame> {
        if let Some(stale) = stale(self, position.stamp()) {
            return stale;
        }
        if position.value().kind() != self.axis().kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        let MapPosition::Asset(frame) = *position.value() else {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry);
        };
        if self.outside_asset_extent(*position.value()) {
            return BeatGridQuery::OutsideDomain;
        }
        BeatGridQuery::Resolved(frame)
    }

    fn revision(&self) -> BeatGridRevision {
        self.revision
    }

    fn state(&self) -> BeatGridState {
        self.state
    }

    fn tempo_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>> {
        if let Some(stale) = stale(self, position.stamp()) {
            return stale;
        }
        if position.value().kind() != self.axis().kind() {
            return BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch);
        }
        if self.outside_asset_extent(*position.value()) {
            return BeatGridQuery::OutsideDomain;
        }
        let Some((tempo, evidence, uncertainty)) = self
            .segments
            .by_position(*position.value())
            .and_then(|segment| segment.tempo_at(self.axis(), *position.value()))
        else {
            return self.missing_position(*position.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            tempo,
            evidence,
            uncertainty,
            self.stamp(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::SegmentGridView;
    use crate::{
        AssetAxis, AssetFrame, Beat, BeatEvidence, BeatGridId, BeatGridQuery, BeatGridRegion,
        BeatGridRevision, BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatGridUnavailable,
        BeatGridView, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis, MapPoint, MapPosition,
        MapRegion, MapRegionError, MapSegment, Meter, MeterFacts, SegmentError, SegmentFacts,
        SegmentSet, SessionAnchor, SessionAxis, SessionBeat, SessionEpoch, SessionFrame,
        beat_grid::session::SessionGridView,
    };

    struct Consts;

    impl Consts {
        const AFTER_EOF_FRAME: f64 = 48_000.5;
        const EOF_FRAME: f64 = 48_000.0;
        const FRAME_COUNT: u64 = 48_000;
        const SAMPLE_RATE: u32 = 48_000;
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("invariant: fixture sample rate is non-zero")
    }

    fn asset_frame(value: f64) -> AssetFrame {
        AssetFrame::new(value).expect("invariant: fixture asset frame is finite and non-negative")
    }

    fn asset_marker(frame: f64, ordinal: i64) -> BeatMarker {
        BeatMarker::new(
            MapPosition::Asset(asset_frame(frame)),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::ZERO,
        )
    }

    fn assert_relation(
        view: &dyn BeatGridView,
        position: MapPosition,
        expected_position: MapPosition,
        expected_region: BeatGridRegion,
    ) {
        let stamp = view.stamp();
        let position = MapPoint::new(stamp, position);
        let region = view.region_at(position);
        let BeatGridQuery::Resolved(beat) = view.beat_at(position) else {
            panic!("the middle native frame must resolve");
        };
        assert_eq!(f64::from(*beat.value().value()), 1.0);

        let BeatGridQuery::Resolved(tempo) = view.tempo_at(position) else {
            panic!("the middle native frame must carry tempo");
        };
        let bpm: f64 = (*tempo.value()).into();
        assert_eq!(bpm, 120.0);

        assert_eq!(
            view.rate_at(position),
            BeatGridQuery::Resolved(1.0),
            "a grid that describes its own recording applies no ratio to it"
        );

        let foreign = BeatGridStamp::new(
            view.id(),
            view.revision()
                .checked_next()
                .expect("invariant: the fixture revision has a successor"),
        );
        assert_eq!(
            view.rate_at(MapPoint::new(foreign, *position.value())),
            BeatGridQuery::Stale {
                expected: stamp,
                given: foreign
            },
            "a rate is answered only for the revision it was asked on"
        );

        let middle_beat = MapPoint::new(
            stamp,
            Beat::new(1.0).expect("invariant: fixture beat is finite"),
        );
        let BeatGridQuery::Resolved(resolved) = view.position_at(middle_beat) else {
            panic!("the middle beat must resolve");
        };
        assert_eq!(*resolved.value().value(), expected_position);

        let BeatGridQuery::Resolved(meter) = view.meter_at(middle_beat) else {
            panic!("the middle beat must carry meter");
        };
        assert_eq!(meter.value().beats_per_bar(), 4);

        assert_eq!(region, BeatGridQuery::Resolved(expected_region));
    }

    #[kithara::test]
    fn session_and_segment_views_obey_the_same_query_protocol() {
        let revision = BeatGridRevision::first();
        let epoch = SessionEpoch::new(7);
        let meter = Meter::new(4).expect("invariant: fixture meter is non-zero");
        let meter_facts = MeterFacts::new(meter, BeatEvidence::Declared, FrameUncertainty::ZERO);
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("invariant: fixture beat is finite"),
            2.0,
            SessionAxis::new(sample_rate(), SessionEpoch::new(0)),
        )
        .expect("invariant: fixture tempo is invertible");
        let session = SessionGridView::new(
            BeatGridId::allocate().expect("invariant: session grid id can be allocated"),
            revision,
            epoch,
            anchor,
            Some(meter_facts),
        );

        let segment = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(48_000.0, 2),
            SegmentFacts::new(
                BeatEvidence::Declared,
                FrameUncertainty::ZERO,
                Some(meter_facts),
            ),
        )
        .expect("invariant: fixture markers form an increasing relation");
        let segments = SegmentSet::new(
            MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
            vec![segment],
        )
        .expect("invariant: fixture segments are valid");
        let segment = SegmentGridView::new(
            BeatGridId::allocate().expect("invariant: segment grid id can be allocated"),
            revision,
            BeatGridState::Complete,
            segments,
        )
        .expect("invariant: complete segment geometry is valid on the asset axis");

        assert_relation(
            &session,
            MapPosition::Session(SessionFrame::new(24_000)),
            MapPosition::Session(SessionFrame::new(24_000)),
            BeatGridRegion::Unbounded,
        );
        assert_relation(
            &segment,
            MapPosition::Asset(asset_frame(24_000.0)),
            MapPosition::Asset(asset_frame(24_000.0)),
            BeatGridRegion::Bounded(MapRegion::between(
                MapPosition::Asset(asset_frame(0.0)),
                MapPosition::Asset(asset_frame(48_000.0)),
            )),
        );
    }

    #[kithara::test]
    fn an_analysed_grid_refuses_a_rate_it_has_no_geometry_for() {
        let segment = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(24_000.0, 1),
            SegmentFacts::new(BeatEvidence::Declared, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing relation");
        let segments = SegmentSet::new(
            MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
            vec![segment],
        )
        .expect("invariant: fixture segments are valid");
        let grid = SegmentGridView::new(
            BeatGridId::allocate().expect("invariant: segment grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            segments,
        )
        .expect("invariant: complete segment geometry is valid on the asset axis");
        let stamp = grid.stamp();

        assert_eq!(
            grid.rate_at(MapPoint::new(
                stamp,
                MapPosition::Asset(asset_frame(Consts::AFTER_EOF_FRAME))
            )),
            BeatGridQuery::OutsideDomain,
            "a position past the recording carries no ratio"
        );
        assert_eq!(
            grid.rate_at(MapPoint::new(
                stamp,
                MapPosition::Session(SessionFrame::new(0))
            )),
            BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch),
            "a rate is answered only on the axis the grid describes"
        );
        assert_eq!(
            grid.source_at(MapPoint::new(
                stamp,
                MapPosition::Asset(asset_frame(12_000.0))
            )),
            BeatGridQuery::Resolved(asset_frame(12_000.0)),
            "a grid that describes its own recording answers with the frame it was asked about"
        );
    }

    #[kithara::test]
    fn complete_asset_grid_round_trips_a_segment_endpoint_at_eof() {
        let asset_axis = AssetAxis::new(sample_rate(), Consts::FRAME_COUNT);
        let eof = asset_frame(Consts::EOF_FRAME);
        assert!(
            !asset_axis.contains(eof),
            "the EOF boundary is not an addressable source sample"
        );
        let segment = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(Consts::EOF_FRAME, 2),
            SegmentFacts::new(BeatEvidence::Interpolated, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation");
        let segments = SegmentSet::new(MapAxis::Asset(asset_axis), vec![segment])
            .expect("a continuous segment may end exactly at the exclusive asset boundary");
        let grid = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            segments,
        )
        .expect("invariant: complete state is valid for a bounded asset grid");
        let endpoint_beat = Beat::new(2.0).expect("invariant: fixture beat is finite");

        let BeatGridQuery::Resolved(position) =
            grid.position_at(MapPoint::new(grid.stamp(), endpoint_beat))
        else {
            panic!("the endpoint beat must resolve to the EOF boundary");
        };
        assert_eq!(*position.value().value(), MapPosition::Asset(eof));

        let BeatGridQuery::Resolved(round_tripped) = grid.beat_at(*position.value()) else {
            panic!("the EOF boundary must resolve through its segment geometry");
        };
        assert_eq!(*round_tripped.value().value(), endpoint_beat);

        let beyond_eof = asset_frame(Consts::AFTER_EOF_FRAME);
        assert!(matches!(
            grid.beat_at(MapPoint::new(grid.stamp(), MapPosition::Asset(beyond_eof),)),
            BeatGridQuery::OutsideDomain
        ));
        let beyond_segment = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(Consts::AFTER_EOF_FRAME, 2),
            SegmentFacts::new(BeatEvidence::Interpolated, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: the overlong fixture remains an affine relation");
        assert_eq!(
            SegmentSet::new(MapAxis::Asset(asset_axis), vec![beyond_segment]),
            Err(SegmentError::OutsideExtent { index: 0 })
        );
    }

    /// A grid the analysis marked only in part answers everywhere.
    ///
    /// The unmarked spans are cut into beats of their neighbours' spacing, so
    /// A set answers the beats it states, each exactly once.
    ///
    /// The overlay and the tick counter consume a grid as a sequence of beats,
    /// so a seam between segments must not repeat the beat it carries and the
    /// final beat of the set must not be dropped.
    #[kithara::test]
    fn a_set_lists_every_whole_beat_once_including_the_last() {
        let first = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(12_000.0, 1),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation");
        let second = MapSegment::new(
            asset_marker(36_000.0, 3),
            asset_marker(42_000.0, 4),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation");
        let set = SegmentSet::new(
            MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
            vec![first, second],
        )
        .expect("invariant: the fixture segments are ordered and disjoint");

        let beats: Vec<(Beat, MapPosition)> = set.beats().collect();

        assert_eq!(
            beats,
            [
                (0.0, 0.0),
                (1.0, 12_000.0),
                (2.0, 24_000.0),
                (3.0, 36_000.0),
                (4.0, 42_000.0),
                (5.0, 48_000.0)
            ]
            .map(|(beat, frame)| (
                Beat::new(beat).expect("invariant: fixture beat is finite"),
                MapPosition::Asset(asset_frame(frame))
            ))
            .to_vec(),
            "every whole beat of the extended set is listed once, with its ordinal"
        );
    }

    /// a gap between two marked runs and the tail after the last marker both
    /// resolve, and their answers say they were extended rather than observed.
    #[kithara::test]
    fn a_partly_marked_asset_grid_answers_its_unmarked_spans() {
        let first = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(12_000.0, 1),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation");
        let second = MapSegment::new(
            asset_marker(36_000.0, 3),
            asset_marker(42_000.0, 4),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing affine relation");
        let grid = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            SegmentSet::new(
                MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
                vec![first, second],
            )
            .expect("invariant: the fixture segments are ordered and disjoint"),
        )
        .expect("invariant: complete state is valid for a bounded asset grid");

        for (frame, expected_beat) in [(24_000.0, 2.0), (45_000.0, 4.5)] {
            let answer = match grid.beat_at(MapPoint::new(
                grid.stamp(),
                MapPosition::Asset(asset_frame(frame)),
            )) {
                BeatGridQuery::Resolved(beat) => beat,
                other => panic!("frame {frame} must resolve, got {other:?}"),
            };
            assert_eq!(
                *answer.value().value(),
                Beat::new(expected_beat).expect("invariant: fixture beat is finite")
            );
            assert_eq!(answer.evidence(), BeatEvidence::Extrapolated);
        }
    }

    #[kithara::test]
    fn asset_grid_round_trips_a_nonzero_first_beat() {
        let first = asset_frame(6_000.0);
        let second = asset_frame(30_000.0);
        let segment = MapSegment::new(
            asset_marker(f64::from(first), 0),
            asset_marker(f64::from(second), 1),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("nonzero first beat defines valid source geometry");
        let grid = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            SegmentSet::new(
                MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
                vec![segment],
            )
            .expect("segment set"),
        )
        .expect("asset grid");

        for (frame, expected_beat) in [(6_000.0, 0.0), (18_000.0, 0.5), (30_000.0, 1.0)] {
            let position = MapPoint::new(grid.stamp(), MapPosition::Asset(asset_frame(frame)));
            let beat = match grid.beat_at(position) {
                BeatGridQuery::Resolved(beat) => beat,
                other => panic!("source frame must resolve, got {other:?}"),
            };
            assert_eq!(
                *beat.value().value(),
                Beat::new(expected_beat).expect("finite beat")
            );
            let round_trip = match grid.position_at(*beat.value()) {
                BeatGridQuery::Resolved(position) => position,
                other => panic!("source beat must resolve, got {other:?}"),
            };
            assert_eq!(*round_trip.value(), position);
        }

        let unmarked = match grid.beat_at(MapPoint::new(
            grid.stamp(),
            MapPosition::Asset(asset_frame(5_999.0)),
        )) {
            BeatGridQuery::Resolved(beat) => beat,
            other => panic!("a frame before the first marked beat must resolve, got {other:?}"),
        };
        let unmarked_position =
            MapPoint::new(grid.stamp(), MapPosition::Asset(asset_frame(5_999.0)));
        assert!(
            *unmarked.value().value() < Beat::new(0.0).expect("invariant: zero is a finite beat"),
            "the head reaches back from the first marked beat, so the beats it answers are negative"
        );
        assert_eq!(unmarked.evidence(), BeatEvidence::Extrapolated);
        let round_trip = match grid.position_at(*unmarked.value()) {
            BeatGridQuery::Resolved(position) => position,
            other => panic!("an extrapolated beat must resolve back to its frame, got {other:?}"),
        };
        assert_eq!(*round_trip.value(), unmarked_position);
    }

    #[kithara::test]
    fn an_extrapolated_span_reports_the_meter_it_inherits_as_extrapolated() {
        let meter = Meter::new(4).expect("invariant: fixture meter is non-zero");
        let segment = MapSegment::new(
            asset_marker(0.0, 0),
            asset_marker(24_000.0, 1),
            SegmentFacts::new(
                BeatEvidence::Observed,
                FrameUncertainty::ZERO,
                Some(MeterFacts::new(
                    meter,
                    BeatEvidence::Observed,
                    FrameUncertainty::ZERO,
                )),
            ),
        )
        .expect("invariant: the fixture segment spans one whole beat");
        let grid = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            SegmentSet::new(
                MapAxis::Asset(AssetAxis::new(sample_rate(), Consts::FRAME_COUNT)),
                vec![segment],
            )
            .expect("invariant: one marked segment extends over the rest of the axis"),
        )
        .expect("invariant: fixture asset grid is valid");

        let marked = MapPoint::new(
            grid.stamp(),
            Beat::new(0.5).expect("invariant: fixture beat is finite"),
        );
        let BeatGridQuery::Resolved(marked) = grid.meter_at(marked) else {
            panic!("the marked span must carry meter");
        };
        assert_eq!(marked.evidence(), BeatEvidence::Observed);

        let tail = MapPoint::new(
            grid.stamp(),
            Beat::new(1.5).expect("invariant: fixture beat is finite"),
        );
        let BeatGridQuery::Resolved(tail) = grid.meter_at(tail) else {
            panic!("the extrapolated tail must carry the meter it inherits");
        };
        assert_eq!(*tail.value(), meter);
        assert_eq!(
            tail.evidence(),
            BeatEvidence::Extrapolated,
            "a meter carried into a span no marker describes is no longer observed there"
        );
    }

    #[kithara::test]
    fn uncovered_eof_uses_grid_lifecycle_instead_of_current_geometry() {
        let asset_axis = AssetAxis::new(sample_rate(), Consts::FRAME_COUNT);
        let axis = MapAxis::Asset(asset_axis);
        let eof = MapPosition::Asset(asset_frame(Consts::EOF_FRAME));
        let beyond_eof = MapPosition::Asset(asset_frame(Consts::AFTER_EOF_FRAME));
        let building = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Building,
            SegmentSet::new(axis, Vec::new()).expect("an empty segment set is valid"),
        )
        .expect("invariant: a bounded asset grid may begin without geometry");

        assert!(matches!(
            building.beat_at(MapPoint::new(building.stamp(), eof)),
            BeatGridQuery::Uncovered { .. }
        ));
        assert!(matches!(
            building.tempo_at(MapPoint::new(building.stamp(), eof)),
            BeatGridQuery::Uncovered { .. }
        ));
        assert!(matches!(
            building.beat_at(MapPoint::new(building.stamp(), beyond_eof)),
            BeatGridQuery::OutsideDomain
        ));
        assert!(matches!(
            building.tempo_at(MapPoint::new(building.stamp(), beyond_eof)),
            BeatGridQuery::OutsideDomain
        ));

        let complete = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            SegmentSet::new(axis, Vec::new()).expect("an empty segment set is valid"),
        )
        .expect("invariant: a complete empty asset grid has no covered positions");
        assert!(matches!(
            complete.beat_at(MapPoint::new(complete.stamp(), eof)),
            BeatGridQuery::OutsideDomain
        ));
        assert!(matches!(
            complete.tempo_at(MapPoint::new(complete.stamp(), eof)),
            BeatGridQuery::OutsideDomain
        ));
    }

    #[kithara::test]
    fn public_map_regions_require_one_forward_native_axis() {
        let first = MapPosition::Asset(asset_frame(1.0));
        let last = MapPosition::Asset(asset_frame(2.0));

        assert_eq!(
            MapRegion::try_from(first..=last),
            Ok(MapRegion::between(first, last))
        );
        assert_eq!(
            MapRegion::try_from(last..=first),
            Err(MapRegionError::Reversed)
        );
        assert_eq!(
            MapRegion::try_from(first..=MapPosition::Session(SessionFrame::new(2))),
            Err(MapRegionError::MixedAxes)
        );
    }
}
