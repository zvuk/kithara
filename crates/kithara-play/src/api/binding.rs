use kithara_warp::{
    Beat, BeatEstimate, BeatGridQuery, BeatGridSnapshot, BeatGridStamp, MapAxis, MapPoint,
    MapPosition,
};

use super::SessionBeat;
use crate::api::PlaybackDirection;

/// A track cannot participate in session synchronization.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncUnavailable {
    /// A session-native grid cannot be used as bounded track geometry.
    #[error("track binding requires an asset-native beat grid")]
    AxisMismatch,
    /// The track anchor belongs to another grid identity or revision.
    #[error("track anchor belongs to another grid identity or revision")]
    StaleAnchor {
        expected: BeatGridStamp,
        given: BeatGridStamp,
    },
    /// Composing the binding anchors produced a non-finite coordinate.
    #[error("binding coordinate overflow")]
    CoordinateOverflow,
}

/// Immutable relationship between a session beat anchor and an asset-native track grid.
#[derive(Clone, Debug, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct TrackBinding {
    /// Immutable asset-grid snapshot used by every binding calculation.
    snapshot: BeatGridSnapshot,
    /// Track beat anchoring this binding.
    #[field(get, copy)]
    track_anchor: MapPoint<Beat>,
    /// Audible direction encoded by this binding.
    #[field(get, copy)]
    direction: PlaybackDirection,
    /// Session beat anchoring this binding.
    #[field(get, copy)]
    session_anchor: SessionBeat,
}

impl TrackBinding {
    /// Captures one asset-grid revision for a stable multi-step calculation.
    ///
    /// # Errors
    ///
    /// Returns [`SyncUnavailable::AxisMismatch`] for a session snapshot or
    /// [`SyncUnavailable::StaleAnchor`] when the anchor stamp is not exact.
    pub fn new(
        snapshot: BeatGridSnapshot,
        session_anchor: SessionBeat,
        track_anchor: MapPoint<Beat>,
        direction: PlaybackDirection,
    ) -> Result<Self, SyncUnavailable> {
        if !matches!(snapshot.axis(), MapAxis::Asset(_)) {
            return Err(SyncUnavailable::AxisMismatch);
        }
        let expected = snapshot.stamp();
        let given = track_anchor.stamp();
        if given != expected {
            return Err(SyncUnavailable::StaleAnchor { expected, given });
        }
        Ok(Self {
            snapshot,
            track_anchor,
            direction,
            session_anchor,
        })
    }

    /// Resolves a session beat through the captured asset-grid revision.
    pub fn position_at(
        &self,
        session_beat: SessionBeat,
    ) -> Result<BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>, SyncUnavailable> {
        Ok(self.snapshot.position_at(self.track_beat_at(session_beat)?))
    }

    /// Returns the stamped asset-grid beat corresponding to this session beat.
    pub fn track_beat_at(
        &self,
        session_beat: SessionBeat,
    ) -> Result<MapPoint<Beat>, SyncUnavailable> {
        let delta = f64::from(session_beat) - f64::from(self.session_anchor);
        let anchor = f64::from(*self.track_anchor.value());
        let value = match self.direction {
            PlaybackDirection::Forward => anchor + delta,
            PlaybackDirection::Reverse => anchor - delta,
        };
        Beat::new(value)
            .map(|beat| MapPoint::new(self.track_anchor.stamp(), beat))
            .map_err(|_| SyncUnavailable::CoordinateOverflow)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use kithara_warp::{
        AssetAxis, AssetFrame, Beat, BeatEvidence, BeatGridId, BeatGridQuery, BeatGridRevision,
        BeatGridSnapshot, BeatGridState, BeatGridUnavailable, BeatMarker, BeatOrdinal,
        FrameUncertainty, MapAxis, MapPoint, MapPosition, MapSegment, SegmentFacts, SegmentSet,
        SessionAnchor, SessionAxis, SessionEpoch, SessionFrame,
    };

    use super::{SessionBeat, SyncUnavailable, TrackBinding};
    use crate::api::PlaybackDirection;

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(48_000).expect("invariant: fixture sample rate is non-zero")
    }

    fn grid_id() -> BeatGridId {
        BeatGridId::allocate().expect("invariant: fixture grid identity space is available")
    }

    fn session_beat(value: f64) -> SessionBeat {
        SessionBeat::new(value).expect("invariant: fixture session beat is finite")
    }

    fn beat(value: f64) -> Beat {
        Beat::new(value).expect("invariant: fixture beat is finite")
    }

    fn marker(frame: f64, ordinal: i64) -> BeatMarker {
        BeatMarker::new(
            AssetFrame::new(frame)
                .expect("invariant: fixture asset frame is valid")
                .into(),
            Some(BeatOrdinal::new(ordinal)),
            BeatEvidence::Observed,
            FrameUncertainty::new(0.0).expect("invariant: exact fixture uncertainty is valid"),
        )
    }

    fn snapshot() -> BeatGridSnapshot {
        let segment = MapSegment::new(
            marker(0.0, 0),
            marker(96_000.0, 2),
            SegmentFacts::new(
                BeatEvidence::Interpolated,
                FrameUncertainty::new(0.0).expect("invariant: exact fixture uncertainty is valid"),
                None,
            ),
        )
        .expect("invariant: fixture segment is valid");
        let segments = SegmentSet::new(
            MapAxis::Asset(AssetAxis::new(sample_rate(), 144_001)),
            vec![segment],
        )
        .expect("invariant: fixture asset topology is valid");
        BeatGridSnapshot::segments(
            grid_id(),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            segments,
        )
        .expect("invariant: fixture asset grid is valid")
    }

    fn binding(direction: PlaybackDirection) -> TrackBinding {
        let snapshot = snapshot();
        let track_anchor = MapPoint::new(snapshot.stamp(), beat(1.0));
        TrackBinding::new(snapshot, session_beat(10.0), track_anchor, direction)
            .expect("invariant: fixture binding uses an asset grid")
    }

    #[kithara::test]
    fn forward_and_reverse_binding_preserve_existing_coordinate_behavior() {
        let forward = binding(PlaybackDirection::Forward);

        assert_eq!(
            forward
                .track_beat_at(session_beat(9.5))
                .map(|point| *point.value()),
            Ok(beat(0.5))
        );
        assert_eq!(
            forward
                .track_beat_at(session_beat(10.5))
                .map(|point| *point.value()),
            Ok(beat(1.5))
        );
        let reverse = binding(PlaybackDirection::Reverse);

        assert_eq!(
            reverse
                .track_beat_at(session_beat(9.5))
                .map(|point| *point.value()),
            Ok(beat(1.5))
        );
        assert_eq!(
            reverse
                .track_beat_at(session_beat(10.5))
                .map(|point| *point.value()),
            Ok(beat(0.5))
        );
    }

    #[kithara::test]
    fn grid_queries_keep_typed_outside_domain_results() {
        let binding = binding(PlaybackDirection::Forward);

        assert!(matches!(
            binding.position_at(session_beat(8.0)),
            Ok(BeatGridQuery::OutsideDomain)
        ));
        assert!(matches!(
            binding.position_at(session_beat(12.0)),
            Ok(BeatGridQuery::OutsideDomain)
        ));
    }

    #[kithara::test]
    fn session_grid_cannot_become_an_asset_track_binding() {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            session_beat(0.0),
            2.0,
            SessionAxis::new(sample_rate(), SessionEpoch::new(0)),
        )
        .expect("invariant: fixture session anchor is valid");
        let session_grid = BeatGridSnapshot::session(
            grid_id(),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        );

        assert!(matches!(
            TrackBinding::new(
                session_grid.clone(),
                session_beat(0.0),
                MapPoint::new(session_grid.stamp(), beat(0.0)),
                PlaybackDirection::Forward,
            ),
            Err(SyncUnavailable::AxisMismatch)
        ));
    }

    #[kithara::test]
    fn binding_rejects_old_and_foreign_track_anchor_stamps() {
        let id = grid_id();
        let axis = MapAxis::Asset(AssetAxis::new(sample_rate(), 96_001));
        let old = BeatGridSnapshot::unavailable(id, BeatGridRevision::first(), axis);
        let revision = old
            .revision()
            .checked_next()
            .expect("invariant: fixture revision can advance");
        let current = BeatGridSnapshot::unavailable(id, revision, axis);
        let old_anchor = MapPoint::new(old.stamp(), beat(1.0));

        assert!(matches!(
            TrackBinding::new(
                current.clone(),
                session_beat(0.0),
                old_anchor,
                PlaybackDirection::Forward,
            ),
            Err(SyncUnavailable::StaleAnchor { expected, given })
                if expected == current.stamp() && given == old.stamp()
        ));

        let foreign = BeatGridSnapshot::unavailable(grid_id(), BeatGridRevision::first(), axis);
        assert!(matches!(
            TrackBinding::new(
                current.clone(),
                session_beat(0.0),
                MapPoint::new(foreign.stamp(), beat(1.0)),
                PlaybackDirection::Forward,
            ),
            Err(SyncUnavailable::StaleAnchor { expected, given })
                if expected == current.stamp() && given == foreign.stamp()
        ));
    }

    #[kithara::test]
    fn unavailable_geometry_stays_a_typed_binding_query_result() {
        let unavailable = BeatGridSnapshot::unavailable(
            grid_id(),
            BeatGridRevision::first(),
            MapAxis::Asset(AssetAxis::new(sample_rate(), 96_001)),
        );
        let binding = TrackBinding::new(
            unavailable.clone(),
            session_beat(0.0),
            MapPoint::new(unavailable.stamp(), beat(0.0)),
            PlaybackDirection::Forward,
        )
        .expect("invariant: unavailable geometry still has an asset axis");

        assert!(matches!(
            binding.position_at(session_beat(0.0)),
            Ok(BeatGridQuery::Unavailable(BeatGridUnavailable::NoGeometry))
        ));
    }

    #[kithara::test]
    fn non_finite_composition_is_coordinate_overflow() {
        let snapshot = snapshot();
        let binding = TrackBinding::new(
            snapshot.clone(),
            session_beat(-f64::MAX),
            MapPoint::new(snapshot.stamp(), beat(0.0)),
            PlaybackDirection::Forward,
        )
        .expect("invariant: fixture binding uses an asset grid");

        assert_eq!(
            binding.track_beat_at(session_beat(f64::MAX)),
            Err(SyncUnavailable::CoordinateOverflow)
        );
    }

    #[kithara::test]
    fn resolved_position_stays_on_the_captured_asset_axis() {
        let binding = binding(PlaybackDirection::Forward);
        let result = binding
            .position_at(session_beat(10.5))
            .expect("invariant: fixture composition is finite");
        let BeatGridQuery::Resolved(estimate) = result else {
            panic!("expected resolved asset position")
        };

        assert_eq!(
            *estimate.value().value(),
            MapPosition::Asset(
                AssetFrame::new(72_000.0).expect("invariant: fixture asset frame is valid")
            )
        );
    }
}
