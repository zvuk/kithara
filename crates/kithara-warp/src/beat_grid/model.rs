//! Materializes a served beat grid as the geometry a deck queries.
//!
//! The served model states beats in media seconds; a deck asks in decoded
//! frames. The translation happens once, here, against the axis the decoder
//! actually produced: the model is never rewritten, and a later change of
//! output rate reprojects the same source geometry instead of recomputing it.
//!
//! The model's `duration` and the decoded axis are two statements about the
//! same recording. Neither overrides the other: the geometry reaches only as
//! far as both allow, rounded down to the last whole beat that fits, so a
//! disagreement between them narrows what the grid claims rather than being
//! silently resolved in favour of one of them.

use kithara_beat::{BeatGridModel, BeatGridState as WireState, GridBeat, RawBeatGrid};
use num_traits::cast::ToPrimitive;

use super::{BeatGridId, BeatGridRevision, BeatGridSnapshot, BeatGridSnapshotError, BeatGridState};
use crate::{
    AssetAxis, AssetExtent, AssetFrame, BeatEvidence, BeatMarker, BeatOrdinal, FrameUncertainty,
    MapAxis, MapCoordinateError, MapSegment, Meter, MeterError, MeterFacts, SegmentError,
    SegmentFacts, SegmentSet, consts,
};

/// A served beat grid cannot be expressed on the decoded axis it was given.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BeatGridModelError {
    /// A beat of the model falls past the decoded asset it describes.
    #[error("beat {ordinal} of the model falls outside the decoded asset")]
    OutsideExtent { ordinal: i64 },
    /// The stated tempo cannot be expressed as a beat length in frames.
    #[error("the stated tempo has no representable beat length in frames")]
    Tempo,
    /// A beat ordinal cannot be extended without leaving its own range.
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
    /// Materializes a served beat grid on one decoded-source axis.
    ///
    /// Marked beats carry the geometry: consecutive ordinals state the exact
    /// interval tempo between them, and the spans before the first and after
    /// the last marker are extended at the stated BPM. The two are never
    /// averaged, so a marked run keeps the tempo it was measured at.
    ///
    /// # Errors
    ///
    /// Returns [`BeatGridModelError`] when a beat of the model falls outside
    /// the decoded asset, when the stated tempo has no representable beat
    /// length, or when the resulting geometry is not a valid segment set.
    pub fn model(
        id: BeatGridId,
        revision: BeatGridRevision,
        model: &BeatGridModel,
        axis: AssetAxis,
    ) -> Result<Self, BeatGridModelError> {
        let raw = model.as_raw();
        let rate = f64::from(axis.sample_rate().get());
        let beat_frames = rate * consts::MODEL_SECONDS_PER_MINUTE / raw.bpm;
        let meter = meter_facts(raw)?;
        let anchors = anchors(raw, axis, rate)?;
        let segments = segments(
            &anchors,
            beat_frames,
            horizon(axis, raw.duration, rate),
            meter,
        )?;
        Self::segments(
            id,
            revision,
            state(raw.state, axis),
            SegmentSet::new(MapAxis::Asset(axis), segments)?,
        )
        .map_err(BeatGridModelError::from)
    }
}

/// One beat of the model placed on the decoded axis.
#[derive(Clone, Copy, Debug)]
struct Anchor {
    evidence: BeatEvidence,
    frame: f64,
    ordinal: i64,
}

/// The model's beats on the decoded axis, or the origin a BPM-only grid
/// assumes.
///
/// A grid that states nothing but a tempo has a geometry and no proven phase,
/// so the assumed origin enters as extrapolated evidence and the bar phase it
/// implies stays unproven.
fn anchors(
    raw: &RawBeatGrid,
    axis: AssetAxis,
    rate: f64,
) -> Result<Vec<Anchor>, BeatGridModelError> {
    if raw.beats.is_empty() {
        return Ok(vec![Anchor {
            ordinal: 0,
            frame: 0.0,
            evidence: BeatEvidence::Extrapolated,
        }]);
    }
    raw.beats
        .iter()
        .map(|beat| anchor(*beat, axis, rate))
        .collect()
}

/// One beat of the model on the decoded axis.
///
/// A beat nothing observed was placed by fitting, so its presence in the
/// served array is no evidence that anything was heard there.
fn anchor(beat: GridBeat, axis: AssetAxis, rate: f64) -> Result<Anchor, BeatGridModelError> {
    let frame = beat.at * rate;
    if !axis.contains_or_eof(AssetFrame::new(frame)?) {
        return Err(BeatGridModelError::OutsideExtent {
            ordinal: beat.ordinal,
        });
    }
    Ok(Anchor {
        frame,
        ordinal: beat.ordinal,
        evidence: if beat.confidence.is_some() {
            BeatEvidence::Observed
        } else {
            BeatEvidence::Interpolated
        },
    })
}

/// The frame past which nothing is claimed.
///
/// Both the stated duration and the decoded extent bound the geometry, so the
/// horizon is whichever of them arrives first; with neither, the grid reaches
/// no further than its last marked beat.
fn horizon(axis: AssetAxis, duration: Option<f64>, rate: f64) -> Option<f64> {
    let extent = match axis.extent() {
        AssetExtent::Bounded(frame_count) => frame_count.to_f64(),
        AssetExtent::Unknown => None,
    };
    let stated = duration.map(|seconds| seconds * rate);
    match (extent, stated) {
        (Some(extent), Some(stated)) => Some(extent.min(stated)),
        (bound, None) | (None, bound) => bound,
    }
}

/// The meter the model states, with the evidence its downbeats give it.
fn meter_facts(raw: &RawBeatGrid) -> Result<Option<(Meter, BeatEvidence)>, BeatGridModelError> {
    let Some(stated) = raw.meter else {
        return Ok(None);
    };
    let meter = Meter::new(stated.beats_per_bar.get())?
        .with_downbeat(BeatOrdinal::new(stated.origin_beat_ordinal));
    let evidence = if raw.downbeats.iter().any(|it| it.confidence.is_some()) {
        BeatEvidence::Observed
    } else if raw.downbeats.is_empty() {
        BeatEvidence::Extrapolated
    } else {
        BeatEvidence::Interpolated
    };
    Ok(Some((meter, evidence)))
}

/// A bounded grid the producer will not revise is complete; everything else
/// may still gain coverage, and an asset whose end nobody has established
/// cannot be complete whatever the producer says about its own revisions.
fn state(wire: WireState, axis: AssetAxis) -> BeatGridState {
    match (wire, axis.extent()) {
        (WireState::Final, AssetExtent::Bounded(_)) => BeatGridState::Complete,
        _ => BeatGridState::Building,
    }
}

fn segments(
    anchors: &[Anchor],
    beat_frames: f64,
    horizon: Option<f64>,
    meter: Option<(Meter, BeatEvidence)>,
) -> Result<Vec<MapSegment>, BeatGridModelError> {
    if !beat_frames.is_finite() || beat_frames <= 0.0 {
        return Err(BeatGridModelError::Tempo);
    }
    let (Some(first), Some(last)) = (anchors.first(), anchors.last()) else {
        return Ok(Vec::new());
    };
    let mut segments: Vec<MapSegment> = Vec::new();
    segments.extend(head(*first, beat_frames, meter)?);
    for pair in anchors.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let evidence =
            if start.evidence == BeatEvidence::Observed && end.evidence == BeatEvidence::Observed {
                BeatEvidence::Observed
            } else {
                BeatEvidence::Interpolated
            };
        segments.push(MapSegment::new(
            marker(start)?,
            marker(end)?,
            facts(evidence, meter, evidence),
        )?);
    }
    segments.extend(tail(*last, beat_frames, horizon, meter)?);
    Ok(segments)
}

/// The whole beats before the first marked one, at the stated tempo.
fn head(
    first: Anchor,
    beat_frames: f64,
    meter: Option<(Meter, BeatEvidence)>,
) -> Result<Option<MapSegment>, BeatGridModelError> {
    let beats = whole_beats(first.frame / beat_frames)?;
    if beats == 0 {
        return Ok(None);
    }
    let ordinal = first
        .ordinal
        .checked_sub(beats)
        .ok_or(BeatGridModelError::Ordinal {
            ordinal: first.ordinal,
        })?;
    let start = Anchor {
        ordinal,
        frame: beat_frames.mul_add(-beats.to_f64().unwrap_or_default(), first.frame),
        evidence: BeatEvidence::Extrapolated,
    };
    Ok(Some(MapSegment::new(
        marker(start)?,
        marker(first)?,
        extrapolated(meter),
    )?))
}

/// The whole beats after the last marked one, at the stated tempo, reaching no
/// further than the horizon.
fn tail(
    last: Anchor,
    beat_frames: f64,
    horizon: Option<f64>,
    meter: Option<(Meter, BeatEvidence)>,
) -> Result<Option<MapSegment>, BeatGridModelError> {
    let Some(horizon) = horizon.filter(|horizon| *horizon > last.frame) else {
        return Ok(None);
    };
    let beats = whole_beats((horizon - last.frame) / beat_frames)?;
    if beats == 0 {
        return Ok(None);
    }
    let ordinal = last
        .ordinal
        .checked_add(beats)
        .ok_or(BeatGridModelError::Ordinal {
            ordinal: last.ordinal,
        })?;
    let end = Anchor {
        ordinal,
        frame: beat_frames.mul_add(beats.to_f64().unwrap_or_default(), last.frame),
        evidence: BeatEvidence::Extrapolated,
    };
    Ok(Some(MapSegment::new(
        marker(last)?,
        marker(end)?,
        extrapolated(meter),
    )?))
}

fn whole_beats(beats: f64) -> Result<i64, BeatGridModelError> {
    beats
        .floor()
        .to_i64()
        .ok_or(BeatGridModelError::Tempo)
        .map(|beats| beats.max(0))
}

/// The facts of a span the stated tempo extends rather than a marker describes.
///
/// A meter carried into such a span is no longer observed there, however it
/// was established where it was stated.
fn extrapolated(meter: Option<(Meter, BeatEvidence)>) -> SegmentFacts {
    facts(
        BeatEvidence::Extrapolated,
        meter,
        BeatEvidence::Extrapolated,
    )
}

fn facts(
    evidence: BeatEvidence,
    meter: Option<(Meter, BeatEvidence)>,
    meter_evidence: BeatEvidence,
) -> SegmentFacts {
    SegmentFacts::new(
        evidence,
        FrameUncertainty::ZERO,
        meter.map(|(meter, stated)| {
            MeterFacts::new(
                meter,
                weaker(stated, meter_evidence),
                FrameUncertainty::ZERO,
            )
        }),
    )
}

/// The evidence a span may claim: never stronger than the meter was stated
/// with, never stronger than the span it is carried into.
const fn weaker(stated: BeatEvidence, span: BeatEvidence) -> BeatEvidence {
    match (stated, span) {
        (BeatEvidence::Extrapolated, _) | (_, BeatEvidence::Extrapolated) => {
            BeatEvidence::Extrapolated
        }
        _ => stated,
    }
}

fn marker(anchor: Anchor) -> Result<BeatMarker, BeatGridModelError> {
    Ok(BeatMarker::new(
        AssetFrame::new(anchor.frame)?.into(),
        Some(BeatOrdinal::new(anchor.ordinal)),
        anchor.evidence,
        FrameUncertainty::ZERO,
    ))
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use kithara_beat::{GridDownbeat, Meter as WireMeter};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{Beat, BeatGridQuery, BeatGridUnavailable, MapPoint, MapPosition, SessionFrame};

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
        let grid = grid(
            served(vec![beat(4, Some(1.0))]),
            bounded(consts::SAMPLE_RATE, 4.0),
        );

        assert_eq!(
            beat_at(&grid, 96_000.0),
            BeatGridQuery::Resolved(4.0),
            "the single marked beat keeps the ordinal the producer gave it"
        );
        assert_eq!(beat_at(&grid, 24_000.0), BeatGridQuery::Resolved(1.0));
        assert_eq!(beat_at(&grid, 144_000.0), BeatGridQuery::Resolved(6.0));
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
            segments(
                &[Anchor {
                    ordinal: 0,
                    frame: 0.0,
                    evidence: BeatEvidence::Observed,
                }],
                f64::INFINITY,
                Some(48_000.0),
                None,
            )
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
}
