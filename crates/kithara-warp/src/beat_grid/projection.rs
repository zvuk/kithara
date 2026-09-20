use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridSnapshot,
    BeatGridState, BeatGridUnavailable, BeatGridView, view::stale,
};
use crate::{
    AssetFrame, Beat, BeatsPerMinute, MapAxis, MapPoint, MapPosition, MapRegion, Meter,
    SessionFrame,
};

/// A source grid cannot be projected onto the grid it was asked to follow.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum GridProjectionError {
    /// The cue beat has no place on the target grid at the activation frame.
    #[error("the target grid does not place a beat at activation frame {activation:?}")]
    Unalignable { activation: SessionFrame },
    /// A projection carries a recording, so its source describes an asset.
    #[error("a projection source requires an asset axis, got {axis:?}")]
    UnsuitableSource { axis: MapAxis },
    /// A projection is heard live, so its target describes a session.
    #[error("a projection target requires a session axis, got {axis:?}")]
    UnsuitableTarget { axis: MapAxis },
}

/// One recording's beat geometry as it sounds on the grid it follows.
///
/// The projection answers in the target's coordinates and holds no geometry of
/// its own: every answer is recomputed from the two grids at the moment it is
/// asked, so nothing it returns can drift away from either of them. It carries
/// the source's identity and revision, because it describes the same recording
/// and must be refused by the same stamp.
#[derive(Debug)]
pub(super) struct GridProjection {
    source: BeatGridSnapshot,
    target: BeatGridSnapshot,
    offset: f64,
}

impl GridProjection {
    pub(super) fn new(
        source: BeatGridSnapshot,
        target: BeatGridSnapshot,
        cue: Beat,
        activation: SessionFrame,
    ) -> Result<Self, GridProjectionError> {
        if !matches!(source.axis(), MapAxis::Asset(_)) {
            return Err(GridProjectionError::UnsuitableSource {
                axis: source.axis(),
            });
        }
        if !matches!(target.axis(), MapAxis::Session(_)) {
            return Err(GridProjectionError::UnsuitableTarget {
                axis: target.axis(),
            });
        }
        let point = MapPoint::new(target.stamp(), MapPosition::Session(activation));
        let BeatGridQuery::Resolved(target_beat) = target.beat_at(point) else {
            return Err(GridProjectionError::Unalignable { activation });
        };
        let offset = f64::from(*target_beat.value().value()) - f64::from(cue);
        Ok(Self {
            source,
            target,
            offset,
        })
    }

    fn output_position(&self, beat: Beat) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>> {
        let Ok(target_beat) = Beat::new(f64::from(beat) + self.offset) else {
            return BeatGridQuery::OutsideDomain;
        };
        self.target
            .position_at(MapPoint::new(self.target.stamp(), target_beat))
            .and_then(|position| {
                BeatGridQuery::Resolved(BeatEstimate::new(
                    MapPoint::new(self.stamp(), *position.value().value()),
                    position.evidence(),
                    position.uncertainty(),
                    self.stamp(),
                ))
            })
    }

    fn output_region(&self, region: MapRegion) -> BeatGridQuery<BeatGridRegion> {
        self.output_bound(region.start())
            .and_then(|first| {
                self.output_bound(region.end())
                    .and_then(|last| BeatGridQuery::Resolved(MapRegion::between(first, last)))
            })
            .and_then(|region| BeatGridQuery::Resolved(BeatGridRegion::Bounded(region)))
    }

    fn output_bound(&self, bound: MapPosition) -> BeatGridQuery<MapPosition> {
        self.source
            .beat_at(MapPoint::new(self.source.stamp(), bound))
            .and_then(|beat| self.output_position(*beat.value().value()))
            .and_then(|position| BeatGridQuery::Resolved(*position.value().value()))
    }

    fn target_query<T>(
        &self,
        position: MapPoint<MapPosition>,
    ) -> Result<MapPoint<MapPosition>, BeatGridQuery<T>> {
        if let Some(stale) = stale(self, position.stamp()) {
            return Err(stale);
        }
        if !matches!(*position.value(), MapPosition::Session(_)) {
            return Err(BeatGridQuery::Unavailable(
                BeatGridUnavailable::AxisMismatch,
            ));
        }
        Ok(MapPoint::new(self.target.stamp(), *position.value()))
    }
}

impl BeatGridView for GridProjection {
    fn axis(&self) -> MapAxis {
        self.target.axis()
    }

    delegate::delegate! {
        to self.source {
            fn id(&self) -> BeatGridId;
            fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>>;
            fn revision(&self) -> BeatGridRevision;
        }
    }

    fn beat_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        let point = match self.target_query(position) {
            Ok(point) => point,
            Err(refusal) => return refusal,
        };
        self.target.beat_at(point).and_then(|beat| {
            let Ok(source_beat) = Beat::new(f64::from(*beat.value().value()) - self.offset) else {
                return BeatGridQuery::OutsideDomain;
            };
            BeatGridQuery::Resolved(BeatEstimate::new(
                MapPoint::new(self.stamp(), source_beat),
                beat.evidence(),
                beat.uncertainty(),
                self.stamp(),
            ))
        })
    }

    fn position_at(
        &self,
        beat: MapPoint<Beat>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>> {
        if let Some(stale) = stale(self, beat.stamp()) {
            return stale;
        }
        self.output_position(*beat.value())
    }

    fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion> {
        self.source_at(position)
            .and_then(|frame| {
                self.source.region_at(MapPoint::new(
                    self.source.stamp(),
                    MapPosition::Asset(frame),
                ))
            })
            .and_then(|region| match region {
                BeatGridRegion::Unbounded => BeatGridQuery::Resolved(BeatGridRegion::Unbounded),
                BeatGridRegion::Bounded(region) => self.output_region(region),
            })
    }

    fn rate_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<f64> {
        let point = match self.target_query(position) {
            Ok(point) => point,
            Err(refusal) => return refusal,
        };
        self.source_at(position).and_then(|frame| {
            self.source
                .tempo_at(MapPoint::new(
                    self.source.stamp(),
                    MapPosition::Asset(frame),
                ))
                .and_then(|source| {
                    self.target.tempo_at(point).and_then(|target| {
                        BeatGridQuery::Resolved(
                            frames_per_beat(self.source.axis(), *source.value())
                                / frames_per_beat(self.target.axis(), *target.value()),
                        )
                    })
                })
        })
    }

    /// Resolves the source frame that sounds at a stamped output position.
    ///
    /// This is the whole relation the renderer needs: an absolute position
    /// rather than a ratio, so the rounding error of one block is absorbed by
    /// the next instead of accumulating into a phase drift.
    fn source_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<AssetFrame> {
        self.beat_at(position)
            .and_then(|beat| self.source.position_at(*beat.value()))
            .and_then(|position| match *position.value().value() {
                MapPosition::Asset(frame) => BeatGridQuery::Resolved(frame),
                MapPosition::Session(_) => {
                    BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch)
                }
            })
    }

    fn state(&self) -> BeatGridState {
        match self.target.state() {
            BeatGridState::Unavailable(reason) => BeatGridState::Unavailable(reason),
            BeatGridState::Building | BeatGridState::Complete | BeatGridState::Live => {
                self.source.state()
            }
        }
    }

    fn tempo_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<BeatsPerMinute>> {
        let point = match self.target_query(position) {
            Ok(point) => point,
            Err(refusal) => return refusal,
        };
        self.target.tempo_at(point).and_then(|tempo| {
            BeatGridQuery::Resolved(BeatEstimate::new(
                *tempo.value(),
                tempo.evidence(),
                tempo.uncertainty(),
                self.stamp(),
            ))
        })
    }
}

/// Resolves how many frames of one axis carry a single beat at a tempo.
///
/// The ratio a projection applies is a relation between two grids rather than
/// a fact read off one, and it is this quotient on each side that relates
/// them, so neither side can be stated without naming both its axis and the
/// tempo measured on it.
fn frames_per_beat(axis: MapAxis, tempo: BeatsPerMinute) -> f64 {
    f64::from(axis.sample_rate().get()) / f64::from(tempo)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::{GridProjection, GridProjectionError};
    use crate::{
        AssetAxis, AssetFrame, Beat, BeatEvidence, BeatGridId, BeatGridQuery, BeatGridRegion,
        BeatGridRevision, BeatGridSnapshot, BeatGridStamp, BeatGridState, BeatGridUnavailable,
        BeatGridView, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis, MapPoint, MapPosition,
        MapSegment, SegmentFacts, SegmentSet, SessionAnchor, SessionAxis, SessionBeat,
        SessionEpoch, SessionFrame,
    };

    struct Consts;

    impl Consts {
        const BEATS: i64 = 400;
        const HOST_BPM: f64 = 100.0;
        const QUEUE_TEMPOS: [f64; 5] = [124.0, 96.0, 132.0, 74.0, 140.0];
        const SAMPLE_RATE: u32 = 48_000;
        const SECONDS_PER_MINUTE: f64 = 60.0;
    }

    fn sample_rate() -> NonZeroU32 {
        NonZeroU32::new(Consts::SAMPLE_RATE).expect("invariant: fixture sample rate is non-zero")
    }

    fn beat_frames(bpm: f64) -> f64 {
        f64::from(Consts::SAMPLE_RATE) * Consts::SECONDS_PER_MINUTE / bpm
    }

    fn source(bpm: f64) -> BeatGridSnapshot {
        let last = beat_frames(bpm) * Consts::BEATS.to_f64().unwrap_or_default();
        let marker = |frame: f64, ordinal: i64| {
            BeatMarker::new(
                MapPosition::Asset(
                    AssetFrame::new(frame).expect("invariant: fixture frame is finite"),
                ),
                Some(BeatOrdinal::new(ordinal)),
                BeatEvidence::Observed,
                FrameUncertainty::ZERO,
            )
        };
        let segment = MapSegment::new(
            marker(0.0, 0),
            marker(last, Consts::BEATS),
            SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None),
        )
        .expect("invariant: fixture markers form an increasing relation");
        let axis = AssetAxis::new(sample_rate(), last.ceil().to_u64().unwrap_or_default() + 1);
        BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            SegmentSet::new(MapAxis::Asset(axis), vec![segment])
                .expect("invariant: fixture segments are valid"),
        )
        .expect("invariant: complete geometry is valid on the asset axis")
    }

    fn host() -> BeatGridSnapshot {
        let anchor = SessionAnchor::new(
            SessionFrame::new(0),
            SessionBeat::new(0.0).expect("invariant: fixture beat is finite"),
            Consts::HOST_BPM / Consts::SECONDS_PER_MINUTE,
            SessionAxis::new(sample_rate(), SessionEpoch::new(0)),
        )
        .expect("invariant: fixture tempo is invertible");
        BeatGridSnapshot::session(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            SessionEpoch::new(0),
            anchor,
            None,
        )
    }

    fn projection(bpm: f64, cue: f64, activation: i64) -> GridProjection {
        GridProjection::new(
            source(bpm),
            host(),
            Beat::new(cue).expect("invariant: fixture cue is finite"),
            SessionFrame::new(activation),
        )
        .expect("invariant: an asset grid projects onto a live session grid")
    }

    fn output(projection: &GridProjection, frame: i64) -> MapPoint<MapPosition> {
        MapPoint::new(
            projection.stamp(),
            MapPosition::Session(SessionFrame::new(frame)),
        )
    }

    #[kithara::test]
    fn a_projection_holds_its_source_on_the_host_grid_beat_after_beat() {
        let cue = 8.0;
        let activation = 12_000;
        let host_beat_frames = beat_frames(Consts::HOST_BPM);

        for bpm in Consts::QUEUE_TEMPOS {
            let projection = projection(bpm, cue, activation);
            let source_beat_frames = beat_frames(bpm);
            for beat in 0..Consts::BEATS - cue.to_i64().unwrap_or_default() {
                let beat = beat.to_f64().unwrap_or_default();
                let frame = activation
                    + (host_beat_frames * beat)
                        .round()
                        .to_i64()
                        .unwrap_or_default();
                let BeatGridQuery::Resolved(source) =
                    projection.source_at(output(&projection, frame))
                else {
                    panic!("beat {beat} of a {bpm} BPM source must resolve to a source frame");
                };
                let drift = f64::from(source) - (cue + beat) * source_beat_frames;
                assert!(
                    drift.abs() < 1.0,
                    "beat {beat} of a {bpm} BPM source sits {drift} frames off the Host grid"
                );
            }
        }
    }

    #[kithara::test]
    fn a_projection_answers_the_ratio_that_carries_its_source_onto_the_host() {
        for bpm in Consts::QUEUE_TEMPOS {
            let projection = projection(bpm, 0.0, 0);
            let BeatGridQuery::Resolved(rate) = projection.rate_at(output(&projection, 96_000))
            else {
                panic!("a projected {bpm} BPM source carries a ratio");
            };
            let expected = Consts::HOST_BPM / bpm;

            assert!(
                (rate - expected).abs() < 1e-12,
                "a {bpm} BPM source on a {} BPM Host stretches by {rate}, expected {expected}",
                Consts::HOST_BPM
            );
        }
    }

    #[kithara::test]
    fn a_projection_reports_the_tempo_the_listener_hears() {
        let projection = projection(124.0, 0.0, 0);
        let BeatGridQuery::Resolved(tempo) = projection.tempo_at(output(&projection, 96_000))
        else {
            panic!("a projected source carries a tempo");
        };

        assert!(
            (f64::from(*tempo.value()) - Consts::HOST_BPM).abs() < 1e-9,
            "a projected source sounds at the Host tempo, got {}",
            f64::from(*tempo.value())
        );
    }

    #[kithara::test]
    fn a_projection_round_trips_a_source_beat_through_the_output_axis() {
        let projection = projection(124.0, 4.0, 28_800);
        let beat = Beat::new(37.0).expect("invariant: fixture beat is finite");

        let BeatGridQuery::Resolved(position) =
            projection.position_at(MapPoint::new(projection.stamp(), beat))
        else {
            panic!("a source beat must reach the output axis");
        };
        let BeatGridQuery::Resolved(round_trip) = projection.beat_at(*position.value()) else {
            panic!("an output position must name the source beat sounding there");
        };

        assert!(
            (f64::from(*round_trip.value().value()) - 37.0).abs() < 1e-9,
            "the round trip lost the beat, got {}",
            f64::from(*round_trip.value().value())
        );
    }

    #[kithara::test]
    fn a_projection_answers_only_on_the_output_axis_and_only_for_its_own_revision() {
        let projection = projection(124.0, 0.0, 0);
        let stamp = projection.stamp();
        let foreign = BeatGridStamp::new(
            projection.id(),
            projection
                .revision()
                .checked_next()
                .expect("invariant: the fixture revision has a successor"),
        );

        assert_eq!(
            projection.source_at(MapPoint::new(
                stamp,
                MapPosition::Asset(AssetFrame::new(0.0).expect("invariant: zero is a valid frame"))
            )),
            BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch),
            "a projection is asked in output frames, never in the frames it answers with"
        );
        assert_eq!(
            projection.source_at(MapPoint::new(
                foreign,
                MapPosition::Session(SessionFrame::new(0))
            )),
            BeatGridQuery::Stale {
                expected: stamp,
                given: foreign
            },
            "a projection is refused by the stamp of the recording it carries"
        );
    }

    #[kithara::test]
    fn a_projection_refuses_a_pair_of_grids_it_cannot_relate() {
        let cue = Beat::new(0.0).expect("invariant: fixture cue is finite");

        assert_eq!(
            GridProjection::new(host(), host(), cue, SessionFrame::new(0)).err(),
            Some(GridProjectionError::UnsuitableSource {
                axis: host().axis()
            }),
            "a live session clock is no recording to project"
        );
        assert_eq!(
            GridProjection::new(source(124.0), source(124.0), cue, SessionFrame::new(0)).err(),
            Some(GridProjectionError::UnsuitableTarget {
                axis: source(124.0).axis()
            }),
            "a recording is no live axis to project onto"
        );
    }

    #[kithara::test]
    fn a_projection_carries_the_source_segment_boundaries_onto_the_output_axis() {
        let projection = projection(124.0, 0.0, 0);
        let BeatGridQuery::Resolved(region) = projection.region_at(output(&projection, 96_000))
        else {
            panic!("a projected source sits inside a bounded source segment");
        };
        let BeatGridRegion::Bounded(region) = region else {
            panic!("a bounded source segment stays bounded on the output axis");
        };
        let host_span = beat_frames(Consts::HOST_BPM) * Consts::BEATS.to_f64().unwrap_or_default();

        assert_eq!(
            region.start(),
            MapPosition::Session(SessionFrame::new(0)),
            "the source segment begins where its first beat sounds"
        );
        let MapPosition::Session(end) = region.end() else {
            panic!("an output region is measured in output frames");
        };
        let end = i64::from(end).to_f64().unwrap_or_default();
        assert!(
            (end - host_span).abs() < 1.0,
            "the source segment ends after {host_span} output frames, got {end}"
        );
    }

    #[kithara::test]
    fn a_projection_refuses_an_output_frame_before_its_source_begins() {
        let projection = projection(124.0, 0.0, 28_800);

        assert_eq!(
            projection.source_at(output(&projection, 0)),
            BeatGridQuery::OutsideDomain,
            "no frame of the recording sounds before the beat it was cued to"
        );
    }

    #[kithara::test]
    fn a_projection_refuses_an_output_frame_past_the_end_of_its_source() {
        let projection = projection(124.0, 0.0, 0);
        let past_the_end = (beat_frames(Consts::HOST_BPM)
            * (Consts::BEATS + 1).to_f64().unwrap_or_default())
        .round()
        .to_i64()
        .unwrap_or_default();

        assert_eq!(
            projection.source_at(output(&projection, past_the_end)),
            BeatGridQuery::OutsideDomain,
            "a refusal from the recording travels out unchanged"
        );
    }

    #[kithara::test]
    fn a_projection_refuses_an_activation_its_target_places_no_beat_at() {
        let target = BeatGridSnapshot::unavailable(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            MapAxis::Session(SessionAxis::new(sample_rate(), SessionEpoch::new(0))),
        );

        assert_eq!(
            GridProjection::new(
                source(124.0),
                target,
                Beat::new(0.0).expect("invariant: fixture cue is finite"),
                SessionFrame::new(96_000),
            )
            .err(),
            Some(GridProjectionError::Unalignable {
                activation: SessionFrame::new(96_000)
            }),
            "a grid without geometry cannot fix the phase of anything"
        );
    }
}
