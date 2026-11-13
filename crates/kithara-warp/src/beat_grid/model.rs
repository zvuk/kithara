//! Adapts one served document to stamped map queries over sync-owned native geometry.

use std::ops::RangeInclusive;

use kithara_beat::{BeatGridModel, BeatGridState as WireState, RawBeatGrid};
use kithara_sync::{SourceGrid, SourceGridError};

use super::{
    BeatEstimate, BeatGridId, BeatGridQuery, BeatGridRegion, BeatGridRevision, BeatGridSnapshot,
    BeatGridSnapshotError, BeatGridState, BeatGridUnavailable, BeatGridView, view::stale,
};
use crate::{
    AssetAxis, AssetExtent, AssetFrame, Beat, BeatEvidence, BeatOrdinal, FrameUncertainty, MapAxis,
    MapCoordinateError, MapPoint, MapPosition, MapRegion, Meter, MeterError, SegmentEndpoint,
    SegmentError,
};

/// A served beat grid cannot be expressed on the decoded axis it was given.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BeatGridModelError {
    #[error("beat {ordinal} of the model falls outside the decoded asset")]
    OutsideExtent { ordinal: i64 },
    #[error("the stated tempo has no representable beat length in frames")]
    Tempo,
    #[error("beat ordinal {ordinal} cannot be extended to the stated extent")]
    Ordinal { ordinal: i64 },
    #[error(transparent)]
    Coordinate(#[from] MapCoordinateError),
    #[error(transparent)]
    Meter(#[from] MeterError),
    #[error(transparent)]
    Segment(#[from] SegmentError),
    #[error(transparent)]
    Snapshot(#[from] BeatGridSnapshotError),
}

impl BeatGridSnapshot {
    /// Prepares a served grid on its decoded clock and freezes its map protocol.
    ///
    /// # Errors
    /// Returns an error when geometry, native extent, tempo or meter cannot be represented.
    pub fn model(
        id: BeatGridId,
        revision: BeatGridRevision,
        model: &BeatGridModel,
        axis: AssetAxis,
    ) -> Result<Self, BeatGridModelError> {
        let raw = model.as_raw();
        let frames = match axis.extent() {
            AssetExtent::Bounded(frames) => Some(frames),
            AssetExtent::Unknown => None,
        };
        let state = match (raw.state, frames) {
            (WireState::Final, Some(_)) => BeatGridState::Complete,
            _ => BeatGridState::Building,
        };
        let source = SourceGrid::new(
            model.geometry(),
            axis.sample_rate(),
            frames,
            state == BeatGridState::Complete,
        )
        .map_err(|error| preparation_error(error, axis))?;
        let meter = raw
            .meter
            .map(|meter| {
                Meter::new(meter.beats_per_bar.get())
                    .map(|value| value.with_downbeat(BeatOrdinal::new(meter.origin_beat_ordinal)))
            })
            .transpose()?;
        Self::freeze(ModelGridView {
            id,
            revision,
            axis,
            state,
            source,
            raw: raw.clone(),
            meter,
        })
        .map_err(BeatGridModelError::from)
    }
}

fn preparation_error(error: SourceGridError, axis: AssetAxis) -> BeatGridModelError {
    match error {
        SourceGridError::OutsideExtent { ordinal } => BeatGridModelError::OutsideExtent { ordinal },
        SourceGridError::Ordinal { ordinal } => BeatGridModelError::Ordinal { ordinal },
        SourceGridError::Coordinate => {
            BeatGridModelError::Coordinate(MapCoordinateError::NonFinite)
        }
        SourceGridError::Tempo => BeatGridModelError::Tempo,
        SourceGridError::IncompleteAxis => {
            BeatGridModelError::Snapshot(BeatGridSnapshotError::InvalidState {
                axis: MapAxis::Asset(axis),
                state: BeatGridState::Complete,
            })
        }
        SourceGridError::InexactStartOrdinal { .. } => {
            BeatGridModelError::Segment(SegmentError::InexactOrdinal {
                endpoint: SegmentEndpoint::Start,
            })
        }
        SourceGridError::InexactEndOrdinal { .. } => {
            BeatGridModelError::Segment(SegmentError::InexactOrdinal {
                endpoint: SegmentEndpoint::End,
            })
        }
        SourceGridError::NonIncreasingPosition => {
            BeatGridModelError::Segment(SegmentError::NonIncreasingPosition)
        }
        SourceGridError::NonIncreasingBeat => {
            BeatGridModelError::Segment(SegmentError::NonIncreasingBeat)
        }
        SourceGridError::IntervalOutsideExtent { index } => {
            BeatGridModelError::Segment(SegmentError::OutsideExtent { index })
        }
        SourceGridError::IntervalTempo { index } => {
            BeatGridModelError::Segment(SegmentError::InvalidTempo { index })
        }
    }
}

#[derive(Debug)]
struct ModelGridView {
    id: BeatGridId,
    revision: BeatGridRevision,
    axis: AssetAxis,
    state: BeatGridState,
    source: SourceGrid,
    raw: RawBeatGrid,
    meter: Option<Meter>,
}

impl ModelGridView {
    fn position<T>(&self, position: MapPoint<MapPosition>) -> Result<AssetFrame, BeatGridQuery<T>> {
        if let Some(stale) = stale(self, position.stamp()) {
            return Err(stale);
        }
        let MapPosition::Asset(frame) = *position.value() else {
            return Err(BeatGridQuery::Unavailable(
                BeatGridUnavailable::AxisMismatch,
            ));
        };
        if !self.axis.contains_or_eof(frame) {
            return Err(BeatGridQuery::OutsideDomain);
        }
        Ok(frame)
    }

    fn missing_position<T>(&self, position: MapPosition) -> BeatGridQuery<T> {
        if self.state == BeatGridState::Complete {
            BeatGridQuery::OutsideDomain
        } else {
            BeatGridQuery::Uncovered {
                required: MapRegion::point(position),
            }
        }
    }

    fn missing_beat<T>(&self, beat: f64) -> BeatGridQuery<T> {
        if self.state == BeatGridState::Complete {
            BeatGridQuery::OutsideDomain
        } else {
            let frame = self.source.uncovered_frame_at_beat(beat);
            let position = AssetFrame::new(frame).map_or_else(
                |_| unreachable!("prepared missing boundary is a finite native frame"),
                MapPosition::Asset,
            );
            BeatGridQuery::Uncovered {
                required: MapRegion::point(position),
            }
        }
    }

    fn evidence_at(&self, frame: f64) -> Option<BeatEvidence> {
        let rate = f64::from(self.axis.sample_rate().get());
        // Checked marker times stay ordered after scaling by a positive rate.
        let index = self
            .raw
            .beats
            .partition_point(|beat| beat.at * rate < frame);
        self.raw
            .beats
            .get(index)
            .filter(|beat| beat.at * rate == frame)
            .map(|beat| {
                if beat.confidence.is_some() {
                    BeatEvidence::Observed
                } else {
                    BeatEvidence::Interpolated
                }
            })
    }

    fn frame_evidence(&self, frame: f64, frames: &RangeInclusive<f64>) -> BeatEvidence {
        if let Some(evidence) = self.evidence_at(frame) {
            return evidence;
        }
        match (
            self.evidence_at(*frames.start()),
            self.evidence_at(*frames.end()),
        ) {
            (Some(BeatEvidence::Observed), Some(BeatEvidence::Observed)) => BeatEvidence::Observed,
            (Some(_), Some(_)) => BeatEvidence::Interpolated,
            _ => BeatEvidence::Extrapolated,
        }
    }

    fn interval_evidence(&self, frames: &RangeInclusive<f64>) -> BeatEvidence {
        let first = self
            .raw
            .beats
            .first()
            .map(|beat| beat.at * f64::from(self.axis.sample_rate().get()));
        let last = self
            .raw
            .beats
            .last()
            .map(|beat| beat.at * f64::from(self.axis.sample_rate().get()));
        if first.is_none_or(|first| *frames.start() < first)
            || last.is_none_or(|last| *frames.end() > last)
        {
            return BeatEvidence::Extrapolated;
        }
        if self.evidence_at(*frames.start()) == Some(BeatEvidence::Observed)
            && self.evidence_at(*frames.end()) == Some(BeatEvidence::Observed)
        {
            BeatEvidence::Observed
        } else {
            BeatEvidence::Interpolated
        }
    }

    fn region(frames: &RangeInclusive<f64>) -> MapRegion {
        MapRegion::between(
            MapPosition::Asset(
                AssetFrame::new(*frames.start()).unwrap_or_else(|_| {
                    unreachable!("prepared interval begins on its native axis")
                }),
            ),
            MapPosition::Asset(
                AssetFrame::new(*frames.end())
                    .unwrap_or_else(|_| unreachable!("prepared interval ends on its native axis")),
            ),
        )
    }
}

impl BeatGridView for ModelGridView {
    fn axis(&self) -> MapAxis {
        MapAxis::Asset(self.axis)
    }
    fn id(&self) -> BeatGridId {
        self.id
    }
    fn revision(&self) -> BeatGridRevision {
        self.revision
    }
    fn state(&self) -> BeatGridState {
        self.state
    }

    fn beat_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<MapPoint<Beat>>> {
        let frame = f64::from(match self.position(position) {
            Ok(frame) => frame,
            Err(error) => return error,
        });
        let Some(beat) = self
            .source
            .beat_at_frame(frame)
            .and_then(|beat| Beat::new(beat).ok())
        else {
            return self.missing_position(*position.value());
        };
        let Some((frames, _)) = self.source.interval_at_frame(frame) else {
            return self.missing_position(*position.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), beat),
            self.frame_evidence(frame, &frames),
            FrameUncertainty::ZERO,
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
        let value = f64::from(*beat.value());
        let Some(frame) = self.source.frame_at_beat(value) else {
            return self.missing_beat(value);
        };
        let Some((frames, beats)) = self.source.interval_at_beat(value) else {
            return self.missing_beat(value);
        };
        let position = MapPosition::Asset(
            AssetFrame::new(frame)
                .unwrap_or_else(|_| unreachable!("prepared beat resolves on its native axis")),
        );
        let evidence = if value == *beats.start() {
            self.frame_evidence(*frames.start(), &frames)
        } else if value == *beats.end() {
            self.frame_evidence(*frames.end(), &frames)
        } else {
            self.interval_evidence(&frames)
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            MapPoint::new(self.stamp(), position),
            evidence,
            FrameUncertainty::ZERO,
            self.stamp(),
        ))
    }

    fn tempo_at(
        &self,
        position: MapPoint<MapPosition>,
    ) -> BeatGridQuery<BeatEstimate<crate::BeatsPerMinute>> {
        let frame = f64::from(match self.position(position) {
            Ok(frame) => frame,
            Err(error) => return error,
        });
        let Some(tempo) = self
            .source
            .bpm_at_frame(frame)
            .and_then(|tempo| crate::BeatsPerMinute::try_from(tempo).ok())
        else {
            return self.missing_position(*position.value());
        };
        let Some((frames, _)) = self.source.interval_at_frame(frame) else {
            return self.missing_position(*position.value());
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            tempo,
            self.interval_evidence(&frames),
            FrameUncertainty::ZERO,
            self.stamp(),
        ))
    }

    fn region_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<BeatGridRegion> {
        let frame = f64::from(match self.position(position) {
            Ok(frame) => frame,
            Err(error) => return error,
        });
        self.source.interval_at_frame(frame).map_or_else(
            || self.missing_position(*position.value()),
            |(frames, _)| BeatGridQuery::Resolved(BeatGridRegion::Bounded(Self::region(&frames))),
        )
    }

    fn meter_at(&self, beat: MapPoint<Beat>) -> BeatGridQuery<BeatEstimate<Meter>> {
        if let Some(stale) = stale(self, beat.stamp()) {
            return stale;
        }
        let value = f64::from(*beat.value());
        let Some((frames, _)) = self.source.interval_at_beat(value) else {
            return self.missing_beat(value);
        };
        let Some(meter) = self.meter else {
            return if self.state == BeatGridState::Building {
                BeatGridQuery::Uncovered {
                    required: Self::region(&frames),
                }
            } else {
                BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter)
            };
        };
        let stated = if self
            .raw
            .downbeats
            .iter()
            .any(|beat| beat.confidence.is_some())
        {
            BeatEvidence::Observed
        } else if self.raw.downbeats.is_empty() {
            BeatEvidence::Extrapolated
        } else {
            BeatEvidence::Interpolated
        };
        let evidence = if stated == BeatEvidence::Extrapolated
            || self.interval_evidence(&frames) == BeatEvidence::Extrapolated
        {
            BeatEvidence::Extrapolated
        } else {
            stated
        };
        BeatGridQuery::Resolved(BeatEstimate::new(
            meter,
            evidence,
            FrameUncertainty::ZERO,
            self.stamp(),
        ))
    }

    fn source_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<AssetFrame> {
        match self.position(position) {
            Ok(frame) => BeatGridQuery::Resolved(frame),
            Err(error) => error,
        }
    }

    fn rate_at(&self, position: MapPoint<MapPosition>) -> BeatGridQuery<f64> {
        self.source_at(position)
            .and_then(|_| BeatGridQuery::Resolved(1.0))
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use kithara_beat::{GridBeat, GridDownbeat};
    use kithara_sync::Meter as WireMeter;
    use kithara_test_utils::kithara;
    use num_traits::ToPrimitive;

    use super::*;
    use crate::{
        Beat, BeatGridQuery, BeatGridUnavailable, BeatMarker, MapPoint, MapPosition, MapSegment,
        SegmentFacts, SegmentSet, SessionFrame, consts,
    };

    fn rate(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("invariant: fixture sample rate is non-zero")
    }

    fn beat(ordinal: i64, confidence: Option<f32>) -> GridBeat {
        GridBeat {
            at: ordinal.to_f64().unwrap_or_default() * consts::SECONDS_PER_BEAT,
            ordinal,
            confidence,
        }
    }

    fn served(beats: Vec<GridBeat>) -> RawBeatGrid {
        RawBeatGrid {
            beats,
            schema_version: kithara_beat::SCHEMA_VERSION,
            model_id: "fixture".to_owned(),
            revision: 1,
            state: WireState::Final,
            duration: None,
            bpm: consts::BPM,
            downbeats: Vec::new(),
            meter: None,
        }
    }

    fn checked(raw: RawBeatGrid) -> BeatGridModel {
        BeatGridModel::try_from(raw).expect("invariant: the fixture grid passes wire validation")
    }

    fn grid(raw: RawBeatGrid, axis: AssetAxis) -> BeatGridSnapshot {
        BeatGridSnapshot::model(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            &checked(raw),
            axis,
        )
        .expect("invariant: the fixture grid materializes on its own axis")
    }

    fn bounded(sample_rate: u32, seconds: f64) -> AssetAxis {
        AssetAxis::new(
            rate(sample_rate),
            AssetExtent::Bounded(
                (seconds * f64::from(sample_rate))
                    .to_u64()
                    .unwrap_or_default(),
            ),
        )
    }

    fn beat_at(grid: &BeatGridSnapshot, frame: f64) -> BeatGridQuery<f64> {
        let position = MapPoint::new(
            grid.stamp(),
            MapPosition::Asset(AssetFrame::new(frame).expect("invariant: fixture frame is finite")),
        );
        grid.beat_at(position)
            .and_then(|beat| BeatGridQuery::Resolved(f64::from(*beat.value().value())))
    }

    #[kithara::test]
    fn a_served_grid_answers_through_the_canonical_query_path() {
        let raw = RawBeatGrid {
            downbeats: vec![
                GridDownbeat {
                    at: 1.0,
                    beat_ordinal: 2,
                    confidence: Some(0.9),
                },
                GridDownbeat {
                    at: 3.0,
                    beat_ordinal: 6,
                    confidence: Some(0.9),
                },
            ],
            meter: Some(WireMeter {
                beats_per_bar: NonZeroU16::new(4).expect("invariant: fixture bar is non-zero"),
                origin_beat_ordinal: 2,
            }),
            ..served((0..8).map(|ordinal| beat(ordinal, Some(1.0))).collect())
        };
        let json = serde_json::to_string(&checked(raw)).expect("a checked grid serializes");
        let model: BeatGridModel =
            serde_json::from_str(&json).expect("a served document passes wire validation");
        let grid = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            &model,
            bounded(consts::SAMPLE_RATE, 4.0),
        )
        .expect("invariant: the served grid materializes on its own axis");
        let beat_point = MapPoint::new(
            grid.stamp(),
            Beat::new(3.0).expect("invariant: fixture beat is finite"),
        );

        assert_eq!(beat_at(&grid, 72_000.0), BeatGridQuery::Resolved(3.0));
        let BeatGridQuery::Resolved(position) = grid.position_at(beat_point) else {
            panic!("a marked beat of the served grid must reach its own axis");
        };
        assert_eq!(
            *position.value().value(),
            MapPosition::Asset(
                AssetFrame::new(72_000.0).expect("invariant: fixture frame is finite")
            )
        );
        let BeatGridQuery::Resolved(tempo) = grid.tempo_at(*position.value()) else {
            panic!("a marked span of the served grid must carry its measured tempo");
        };
        assert_eq!(f64::from(*tempo.value()), consts::BPM);
        let BeatGridQuery::Resolved(meter) = grid.meter_at(beat_point) else {
            panic!("a grid whose downbeats were heard must carry its meter");
        };
        assert_eq!(meter.value().beats_per_bar(), 4);
        assert_eq!(
            meter.value().downbeat(),
            BeatOrdinal::new(2),
            "the stated bar origin survives materialization instead of being reset to zero"
        );
        assert_eq!(meter.evidence(), BeatEvidence::Observed);
    }

    #[kithara::test]
    fn a_fitted_beat_is_no_observation_of_one() {
        let grid = grid(
            RawBeatGrid {
                state: WireState::Provisional,
                ..served(vec![
                    beat(0, Some(1.0)),
                    beat(1, None),
                    beat(2, Some(1.0)),
                    beat(3, Some(1.0)),
                ])
            },
            bounded(consts::SAMPLE_RATE, 2.0),
        );

        let BeatGridQuery::Resolved(fitted) = beat_at_estimate(&grid, 6_000.0) else {
            panic!("a provisional grid with anchors answers inside them");
        };
        assert_eq!(
            fitted,
            BeatEvidence::Interpolated,
            "a beat carrying no confidence was placed by fitting, not heard"
        );
        let BeatGridQuery::Resolved(observed) = beat_at_estimate(&grid, 60_000.0) else {
            panic!("the span between two heard beats answers");
        };
        assert_eq!(observed, BeatEvidence::Observed);
    }

    #[kithara::test]
    fn long_served_grids_preserve_marker_and_interval_evidence() {
        let beats: Vec<_> = (0..2_048_u16)
            .map(|index| GridBeat {
                at: 0.123_456_789 + f64::from(index) * 0.5,
                ordinal: i64::from(index),
                confidence: [Some(1.0), None, Some(0.0)][usize::from(index) % 3],
            })
            .collect();
        for sample_rate in [44_100, 48_000] {
            let grid = grid(served(beats.clone()), bounded(sample_rate, 1_025.0));
            let scale = f64::from(sample_rate);
            for beat in &beats {
                let expected = if beat.confidence.is_some() {
                    BeatEvidence::Observed
                } else {
                    BeatEvidence::Interpolated
                };
                assert_eq!(
                    beat_at_estimate(&grid, beat.at * scale),
                    BeatGridQuery::Resolved(expected),
                    "marker {} at rate {sample_rate}",
                    beat.ordinal,
                );
            }
            for pair in beats.windows(2) {
                let frame = (pair[0].at * scale + pair[1].at * scale) / 2.0;
                let position = MapPoint::new(
                    grid.stamp(),
                    MapPosition::Asset(AssetFrame::new(frame).expect("finite midpoint")),
                );
                let expected = if pair.iter().all(|beat| beat.confidence.is_some()) {
                    BeatEvidence::Observed
                } else {
                    BeatEvidence::Interpolated
                };
                assert_eq!(
                    beat_at_estimate(&grid, frame),
                    BeatGridQuery::Resolved(expected)
                );
                let BeatGridQuery::Resolved(tempo) = grid.tempo_at(position) else {
                    panic!("a prepared interval resolves its tempo");
                };
                assert_eq!(tempo.evidence(), expected);
            }
            for frame in [0.0, 1_024.5 * scale] {
                assert_eq!(
                    beat_at_estimate(&grid, frame),
                    BeatGridQuery::Resolved(BeatEvidence::Extrapolated),
                );
            }
        }
    }

    fn beat_at_estimate(grid: &BeatGridSnapshot, frame: f64) -> BeatGridQuery<BeatEvidence> {
        let position = MapPoint::new(
            grid.stamp(),
            MapPosition::Asset(AssetFrame::new(frame).expect("invariant: fixture frame is finite")),
        );
        grid.beat_at(position)
            .and_then(|beat| BeatGridQuery::Resolved(beat.evidence()))
    }

    #[kithara::test]
    fn a_tempo_only_grid_states_a_geometry_and_no_proven_phase() {
        let raw = RawBeatGrid {
            meter: Some(WireMeter {
                beats_per_bar: NonZeroU16::new(4).expect("invariant: fixture bar is non-zero"),
                origin_beat_ordinal: 0,
            }),
            ..served(Vec::new())
        };
        let grid = grid(raw, bounded(consts::SAMPLE_RATE, 2.0));
        let beat = MapPoint::new(
            grid.stamp(),
            Beat::new(1.0).expect("invariant: fixture beat is finite"),
        );

        assert_eq!(beat_at(&grid, 24_000.0), BeatGridQuery::Resolved(1.0));
        let BeatGridQuery::Resolved(evidence) = beat_at_estimate(&grid, 24_000.0) else {
            panic!("a grid extended from its assumed origin still answers");
        };
        assert_eq!(evidence, BeatEvidence::Extrapolated);
        let BeatGridQuery::Resolved(meter) = grid.meter_at(beat) else {
            panic!("the stated meter travels with the geometry it was stated for");
        };
        assert_eq!(
            meter.evidence(),
            BeatEvidence::Extrapolated,
            "a bar nothing marked is claimed no more strongly than the beats under it"
        );
    }

    #[kithara::test]
    fn one_marked_beat_fixes_the_phase_the_tempo_extends() {
        let original = grid(
            served(vec![beat(4, Some(1.0))]),
            bounded(consts::SAMPLE_RATE, 4.0),
        );

        assert_eq!(
            beat_at(&original, 96_000.0),
            BeatGridQuery::Resolved(4.0),
            "the single marked beat keeps the ordinal the producer gave it"
        );
        assert_eq!(beat_at(&original, 24_000.0), BeatGridQuery::Resolved(1.0));
        assert_eq!(beat_at(&original, 144_000.0), BeatGridQuery::Resolved(6.0));

        let first_frame = 20_571.0;
        let partial = grid(
            RawBeatGrid {
                bpm: 140.0,
                ..served(vec![GridBeat {
                    ordinal: 0,
                    at: first_frame / 48_000.0,
                    confidence: Some(1.0),
                }])
            },
            bounded(48_000, 12.0),
        );
        let initial = MapPoint::new(
            partial.stamp(),
            MapPosition::Asset(AssetFrame::new(0.0).expect("asset origin")),
        );
        let BeatGridQuery::Resolved(origin) = partial.beat_at(initial) else {
            panic!("the partial pickup carries extrapolated geometry");
        };
        assert_eq!(origin.evidence(), BeatEvidence::Extrapolated);
        assert_eq!(
            f64::from(*origin.value().value()),
            -first_frame / (48_000.0 * 60.0 / 140.0)
        );
        let BeatGridQuery::Resolved(round_trip) = partial.position_at(*origin.value()) else {
            panic!("the partial beat maps back to the asset origin");
        };
        assert_eq!(*round_trip.value().value(), *initial.value());
        let BeatGridQuery::Resolved(tempo) = partial.tempo_at(initial) else {
            panic!("the pickup preserves the stated tempo");
        };
        assert!((f64::from(*tempo.value()) - 140.0).abs() < 1e-12);
        assert_eq!(beat_at(&partial, first_frame), BeatGridQuery::Resolved(0.0));
        assert_eq!(beat_at(&partial, 576_001.0), BeatGridQuery::OutsideDomain);
    }

    #[kithara::test]
    fn a_sparse_grid_keeps_the_ordinals_its_gaps_span() {
        let grid = grid(
            served(vec![
                beat(0, Some(1.0)),
                beat(1, Some(1.0)),
                beat(4, Some(1.0)),
                beat(5, Some(1.0)),
            ]),
            bounded(consts::SAMPLE_RATE, 4.0),
        );

        assert_eq!(
            beat_at(&grid, 36_000.0),
            BeatGridQuery::Resolved(1.5),
            "a gap between marked ordinals is spanned, never renumbered"
        );
        assert_eq!(beat_at(&grid, 96_000.0), BeatGridQuery::Resolved(4.0));
    }

    #[kithara::test]
    fn a_beat_past_the_decoded_asset_is_a_contradiction_not_a_clamp() {
        let raw = served(vec![beat(0, Some(1.0)), beat(8, Some(1.0))]);

        assert_eq!(
            BeatGridSnapshot::model(
                BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
                BeatGridRevision::first(),
                &checked(raw),
                bounded(consts::SAMPLE_RATE, 2.0),
            )
            .err(),
            Some(BeatGridModelError::OutsideExtent { ordinal: 8 })
        );
    }

    #[kithara::test]
    fn an_unknown_end_is_no_end_of_file() {
        let grid = grid(
            RawBeatGrid {
                state: WireState::Final,
                ..served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))])
            },
            AssetAxis::new(rate(consts::SAMPLE_RATE), AssetExtent::Unknown),
        );

        assert_eq!(
            grid.state(),
            BeatGridState::Building,
            "a recording whose end nobody established cannot be a complete grid"
        );
        assert!(
            matches!(beat_at(&grid, 480_000.0), BeatGridQuery::Uncovered { .. }),
            "past the marked geometry an unbounded grid is uncovered, not outside its domain"
        );
    }

    #[kithara::test]
    fn a_known_extent_stays_the_bound_the_grid_answers_inside() {
        let grid = grid(
            served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))]),
            bounded(consts::SAMPLE_RATE, 2.0),
        );

        assert_eq!(grid.state(), BeatGridState::Complete);
        assert_eq!(beat_at(&grid, 96_000.0), BeatGridQuery::Resolved(4.0));
        assert_eq!(beat_at(&grid, 96_001.0), BeatGridQuery::OutsideDomain);
    }

    #[kithara::test]
    fn a_stated_duration_shorter_than_the_decoded_asset_bounds_the_geometry() {
        let grid = grid(
            RawBeatGrid {
                duration: Some(1.0),
                ..served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))])
            },
            bounded(consts::SAMPLE_RATE, 4.0),
        );

        assert_eq!(beat_at(&grid, 48_000.0), BeatGridQuery::Resolved(2.0));
        assert!(
            matches!(beat_at(&grid, 96_000.0), BeatGridQuery::OutsideDomain),
            "the grid claims nothing past the shorter of the two statements about its length"
        );
    }

    #[kithara::test]
    fn the_same_media_beat_round_trips_on_every_decoded_rate() {
        for sample_rate in consts::RATES {
            let grid = grid(
                served(vec![beat(0, Some(1.0)), beat(4, Some(1.0))]),
                bounded(sample_rate, 4.0),
            );
            let beat = MapPoint::new(
                grid.stamp(),
                Beat::new(2.5).expect("invariant: fixture beat is finite"),
            );

            let BeatGridQuery::Resolved(position) = grid.position_at(beat) else {
                panic!("a beat inside the marked span resolves at {sample_rate} Hz");
            };
            assert_eq!(
                *position.value().value(),
                MapPosition::Asset(
                    AssetFrame::new(1.25 * f64::from(sample_rate))
                        .expect("invariant: fixture frame is finite")
                ),
                "the media time of a beat does not depend on the rate it was decoded at"
            );
            let BeatGridQuery::Resolved(round_trip) = grid.beat_at(*position.value()) else {
                panic!("the frame a beat sounds at names that beat again at {sample_rate} Hz");
            };
            assert_eq!(f64::from(*round_trip.value().value()), 2.5);
        }
    }

    #[kithara::test]
    fn an_output_rate_change_leaves_the_served_model_and_its_source_frames_alone() {
        let raw = served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))]);
        let model = checked(raw);
        let first = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("invariant: fixture grid id can be allocated"),
            BeatGridRevision::first(),
            &model,
            bounded(consts::SAMPLE_RATE, 2.0),
        )
        .expect("invariant: the fixture grid materializes on its own axis");

        assert_eq!(
            beat_at(&first, 24_000.0),
            BeatGridQuery::Resolved(1.0),
            "the source geometry is stated on the decoded axis, never on the output one"
        );
        assert_eq!(
            model,
            checked(served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))])),
            "materialization reads the served model and never rewrites it"
        );
    }

    #[kithara::test]
    fn a_grid_whose_downbeat_misses_its_beats_is_refused_by_the_wire_contract() {
        let raw = RawBeatGrid {
            downbeats: vec![GridDownbeat {
                at: 0.25,
                beat_ordinal: 0,
                confidence: None,
            }],
            ..served(vec![beat(0, Some(1.0)), beat(1, Some(1.0))])
        };

        assert!(
            BeatGridModel::try_from(raw).is_err(),
            "a downbeat that sits on no beat is rejected where the document is checked"
        );
    }

    #[kithara::test]
    fn a_grid_of_one_beat_states_the_geometry_its_tempo_gives_it() {
        let grid = grid(
            served(vec![beat(0, None)]),
            bounded(consts::SAMPLE_RATE, 2.0),
        );

        assert_eq!(
            beat_at(&grid, 24_000.0),
            BeatGridQuery::Resolved(1.0),
            "a single marked beat is no reason to refuse a grid the tempo already states"
        );
    }

    #[kithara::test]
    fn a_tempo_with_no_representable_beat_length_is_refused() {
        assert_eq!(
            SourceGrid::new(
                checked(RawBeatGrid {
                    bpm: f64::MIN_POSITIVE,
                    ..served(vec![beat(0, Some(1.0))])
                })
                .geometry(),
                rate(consts::SAMPLE_RATE),
                Some(48_000),
                true,
            )
            .map_err(|error| preparation_error(
                error,
                AssetAxis::new(rate(consts::SAMPLE_RATE), AssetExtent::Bounded(48_000))
            ))
            .err(),
            Some(BeatGridModelError::Tempo)
        );
    }

    #[kithara::test]
    fn an_unavailable_geometry_stays_typed_on_the_materialized_grid() {
        let grid = grid(
            served(vec![beat(0, Some(1.0)), beat(2, Some(1.0))]),
            bounded(consts::SAMPLE_RATE, 2.0),
        );
        let beat = MapPoint::new(
            grid.stamp(),
            Beat::new(0.0).expect("invariant: fixture beat is finite"),
        );

        assert_eq!(
            grid.meter_at(beat),
            BeatGridQuery::Unavailable(BeatGridUnavailable::NoMeter),
            "a grid that states no meter says so instead of inventing a bar"
        );
        assert_eq!(
            grid.source_at(MapPoint::new(
                grid.stamp(),
                MapPosition::Session(SessionFrame::new(0))
            )),
            BeatGridQuery::Unavailable(BeatGridUnavailable::AxisMismatch)
        );
    }

    #[kithara::test]
    fn served_native_geometry_matches_independent_manual_intervals() {
        for sample_rate in consts::RATES {
            let scale = f64::from(sample_rate);
            let cases = [
                (
                    vec![(0.0, 0), (0.4, 1), (2.2, 4), (2.7, 5)],
                    120.0,
                    Some(4.0),
                    Some(4.0),
                    WireState::Final,
                    vec![
                        (0.0, 0.4, 0.0, 1.0),
                        (0.4, 2.2, 1.0, 4.0),
                        (2.2, 2.7, 4.0, 5.0),
                        (2.7, 3.7, 5.0, 7.0),
                    ],
                ),
                (
                    vec![(0.0, 0), (0.4, 1), (2.2, 4)],
                    120.0,
                    None,
                    None,
                    WireState::Final,
                    vec![(0.0, 0.4, 0.0, 1.0), (0.4, 2.2, 1.0, 4.0)],
                ),
                (
                    vec![(0.0, 0), (0.4, 1), (2.2, 4), (2.7, 5)],
                    120.0,
                    Some(4.0),
                    Some(4.0),
                    WireState::Provisional,
                    vec![
                        (0.0, 0.4, 0.0, 1.0),
                        (0.4, 2.2, 1.0, 4.0),
                        (2.2, 2.7, 4.0, 5.0),
                        (2.7, 3.7, 5.0, 7.0),
                    ],
                ),
                (
                    vec![],
                    120.0,
                    Some(2.0),
                    Some(2.0),
                    WireState::Final,
                    vec![(0.0, 2.0, 0.0, 4.0)],
                ),
                (
                    vec![(2.0, 4)],
                    120.0,
                    Some(4.0),
                    Some(4.0),
                    WireState::Final,
                    vec![(0.0, 2.0, 0.0, 4.0), (2.0, 4.0, 4.0, 8.0)],
                ),
                (
                    vec![(0.0, 0), (1.0, 2)],
                    120.0,
                    Some(1.0),
                    Some(4.0),
                    WireState::Final,
                    vec![(0.0, 1.0, 0.0, 2.0)],
                ),
            ];
            for (beats, bpm, duration, decoded, state, spans) in cases {
                let raw = RawBeatGrid {
                    bpm,
                    duration,
                    state,
                    beats: beats
                        .into_iter()
                        .map(|(at, ordinal)| GridBeat {
                            at,
                            ordinal,
                            confidence: Some(1.0),
                        })
                        .collect(),
                    ..served(Vec::new())
                };
                let axis = AssetAxis::new(
                    rate(sample_rate),
                    decoded.map_or(AssetExtent::Unknown, |seconds| {
                        AssetExtent::Bounded((seconds * scale).to_u64().expect("fixture extent"))
                    }),
                );
                assert_manual_geometry(raw, axis, &spans);
            }
            let first = 20_571.0 / 48_000.0;
            let head_beat = -(first * scale) / (scale * 60.0 / 140.0);
            assert_manual_geometry(
                RawBeatGrid {
                    bpm: 140.0,
                    duration: None,
                    beats: vec![GridBeat {
                        at: first,
                        ordinal: 0,
                        confidence: Some(1.0),
                    }],
                    ..served(Vec::new())
                },
                AssetAxis::new(rate(sample_rate), AssetExtent::Unknown),
                &[(0.0, first, head_beat, 0.0)],
            );
        }
        let axis = AssetAxis::new(rate(48_000), AssetExtent::Unknown);
        let inexact = checked(RawBeatGrid {
            beats: vec![
                GridBeat {
                    at: 0.0,
                    ordinal: 9_007_199_254_740_993,
                    confidence: None,
                },
                GridBeat {
                    at: 0.5,
                    ordinal: 9_007_199_254_740_994,
                    confidence: None,
                },
            ],
            ..served(Vec::new())
        });
        assert_eq!(
            BeatGridSnapshot::model(
                BeatGridId::allocate().expect("grid id"),
                BeatGridRevision::first(),
                &inexact,
                axis
            )
            .err(),
            Some(BeatGridModelError::Segment(SegmentError::InexactOrdinal {
                endpoint: SegmentEndpoint::Start
            }))
        );
        let collapsed = checked(RawBeatGrid {
            beats: vec![
                GridBeat {
                    at: 0.24324324324324326,
                    ordinal: 0,
                    confidence: None,
                },
                GridBeat {
                    at: 0.24324324324324328,
                    ordinal: 1,
                    confidence: None,
                },
            ],
            ..served(Vec::new())
        });
        assert_eq!(
            BeatGridSnapshot::model(
                BeatGridId::allocate().expect("grid id"),
                BeatGridRevision::first(),
                &collapsed,
                axis
            )
            .err(),
            Some(BeatGridModelError::Segment(
                SegmentError::NonIncreasingPosition
            ))
        );
        let model = checked(RawBeatGrid {
            beats: vec![
                GridBeat {
                    at: 1.0,
                    ordinal: 0,
                    confidence: None,
                },
                GridBeat {
                    at: 1.5,
                    ordinal: 1,
                    confidence: Some(1.0),
                },
            ],
            ..served(Vec::new())
        });
        let grid = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            &model,
            axis,
        )
        .expect("mixed evidence");
        let point = MapPoint::new(
            grid.stamp(),
            Beat::new(f64::from_bits(1.0_f64.to_bits() - 1)).expect("interior beat"),
        );
        let BeatGridQuery::Resolved(position) = grid.position_at(point) else {
            panic!("interior inverse resolves");
        };
        assert_eq!(position.evidence(), BeatEvidence::Interpolated);
        assert_eq!(
            *position.value().value(),
            MapPosition::Asset(AssetFrame::new(72_000.0).expect("rounded endpoint"))
        );
        let large = checked(RawBeatGrid {
            beats: vec![
                GridBeat {
                    at: 1e20,
                    ordinal: 0,
                    confidence: None,
                },
                GridBeat {
                    at: 1e20 + 32_768.0,
                    ordinal: 1,
                    confidence: None,
                },
            ],
            ..served(Vec::new())
        });
        let grid = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            &large,
            axis,
        )
        .expect("finite native geometry exceeds Duration without invalidating Warp");
        let point = MapPoint::new(
            grid.stamp(),
            MapPosition::Asset(AssetFrame::new(1e20 * 48_000.0).expect("native frame")),
        );
        assert!(matches!(grid.beat_at(point), BeatGridQuery::Resolved(_)));
    }

    fn assert_manual_geometry(raw: RawBeatGrid, axis: AssetAxis, spans: &[(f64, f64, f64, f64)]) {
        let scale = f64::from(axis.sample_rate().get());
        let model = checked(raw);
        let actual = BeatGridSnapshot::model(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            &model,
            axis,
        )
        .expect("served geometry");
        let marker = |frame, ordinal| {
            BeatMarker::new(
                MapPosition::Asset(AssetFrame::new(frame).expect("native frame")),
                Some(BeatOrdinal::new(ordinal)),
                BeatEvidence::Observed,
                FrameUncertainty::ZERO,
            )
        };
        let segments: Vec<_> = spans
            .iter()
            .map(|&(start, end, first, last)| {
                let facts = SegmentFacts::new(BeatEvidence::Observed, FrameUncertainty::ZERO, None);
                let end_frame = model
                    .as_raw()
                    .beats
                    .last()
                    .filter(|beat| end > beat.at)
                    .map_or(end * scale, |beat| {
                        (scale * 60.0 / model.as_raw().bpm).mul_add(
                            last - beat.ordinal.to_f64().expect("source ordinal"),
                            beat.at * scale,
                        )
                    });
                let end = marker(end_frame, last.to_i64().expect("integral end"));
                if first.fract() == 0.0 {
                    MapSegment::new(
                        marker(start * scale, first.to_i64().expect("integral start")),
                        end,
                        facts,
                    )
                    .expect("manual affine interval")
                } else {
                    crate::segment::partial_start(
                        MapPosition::Asset(AssetFrame::new(start * scale).expect("start")),
                        Beat::new(first).expect("fractional pickup"),
                        end,
                        facts,
                    )
                    .expect("manual pickup")
                }
            })
            .collect();
        let expected = BeatGridSnapshot::segments(
            actual.id(),
            actual.revision(),
            actual.state(),
            SegmentSet::new(MapAxis::Asset(axis), segments).expect("manual segment set"),
        )
        .expect("manual grid");
        let points = spans
            .iter()
            .flat_map(|&(start, end, _, _)| {
                [start * scale, (start + end) * scale / 2.0, end * scale]
            })
            .chain([
                0.0,
                scale * 0.4,
                scale * 2.2,
                scale * 3.8,
                scale * 4.0,
                scale * 4.0 + 1.0,
            ]);
        for frame in points {
            let position = MapPoint::new(
                actual.stamp(),
                MapPosition::Asset(AssetFrame::new(frame).expect("query frame")),
            );
            let value = |answer: BeatGridQuery<BeatEstimate<MapPoint<Beat>>>| {
                answer.and_then(|beat| BeatGridQuery::Resolved(f64::from(*beat.value().value())))
            };
            assert_eq!(
                value(actual.beat_at(position)),
                value(expected.beat_at(position)),
                "native beat at {frame}"
            );
            assert_eq!(
                actual.region_at(position),
                expected.region_at(position),
                "following interval at {frame}"
            );
            let value = |answer: BeatGridQuery<BeatEstimate<crate::BeatsPerMinute>>| {
                answer.and_then(|tempo| BeatGridQuery::Resolved(f64::from(*tempo.value())))
            };
            assert_eq!(
                value(actual.tempo_at(position)),
                value(expected.tempo_at(position)),
                "local tempo at {frame}"
            );
            if let BeatGridQuery::Resolved(beat) = actual.beat_at(position) {
                let point = *beat.value();
                let value = |answer: BeatGridQuery<BeatEstimate<MapPoint<MapPosition>>>| {
                    answer.and_then(|position| BeatGridQuery::Resolved(*position.value().value()))
                };
                assert_eq!(
                    value(actual.position_at(point)),
                    value(expected.position_at(point)),
                    "inverse native coordinate at {frame}"
                );
            }
        }
    }
}
