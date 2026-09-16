//! Expresses a fixture's [`BeatArtifact`] as the warp [`SegmentSet`] a deck
//! publishes for a track. Fixture support: the product publishes grids from
//! its own analysis bridge, not through this module.

use std::{cmp::Reverse, collections::BTreeMap};

use kithara::{
    analysis::BeatArtifact,
    warp::{
        AssetAxis, AssetFrame, BeatEvidence, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis,
        MapCoordinateError, MapSegment, Meter, MeterError, MeterFacts, SegmentError, SegmentFacts,
        SegmentSet,
    },
};
use num_traits::cast::AsPrimitive;

/// Failure to express a [`BeatArtifact`] as a warp [`SegmentSet`].
#[derive(Debug, thiserror::Error)]
pub enum BeatGridError {
    /// A grid needs at least one beat interval.
    #[error("beat artifact carries fewer than two beats")]
    TooFewBeats,
    /// The dominant bar length exceeds the meter range.
    #[error("bar of {beats} beats exceeds the meter range")]
    BarTooLong { beats: usize },
    #[error(transparent)]
    Coordinate(#[from] MapCoordinateError),
    #[error(transparent)]
    Meter(#[from] MeterError),
    #[error(transparent)]
    Segment(#[from] SegmentError),
}

/// One observed segment per beat interval of `artifact` on `axis`. The meter
/// comes from the downbeats that sit on beats: the most frequent beat count
/// between neighbours is the bar length, the most frequent ordinal residue the
/// bar phase, so an irregular intro bar or a stray downbeat does not set it.
pub fn segment_set(artifact: &BeatArtifact, axis: AssetAxis) -> Result<SegmentSet, BeatGridError> {
    if artifact.beats().len() < 2 {
        return Err(BeatGridError::TooFewBeats);
    }
    let exact = FrameUncertainty::new(0.0)?;
    let facts = SegmentFacts::new(
        BeatEvidence::Observed,
        exact,
        meter(artifact)?.map(|meter| MeterFacts::new(meter, BeatEvidence::Observed, exact)),
    );
    let marker = |ordinal: usize, frame: u64| -> Result<BeatMarker, BeatGridError> {
        let frame: f64 = frame.as_();
        Ok(BeatMarker::new(
            AssetFrame::new(frame)?.into(),
            Some(BeatOrdinal::new(ordinal.as_())),
            BeatEvidence::Observed,
            exact,
        ))
    };
    let segments = artifact
        .beats()
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            Ok(MapSegment::new(
                marker(index, pair[0])?,
                marker(index + 1, pair[1])?,
                facts,
            )?)
        })
        .collect::<Result<Vec<_>, BeatGridError>>()?;
    Ok(SegmentSet::new(MapAxis::Asset(axis), segments)?)
}

fn meter(artifact: &BeatArtifact) -> Result<Option<Meter>, BeatGridError> {
    let beats = artifact.beats();
    let ordinals: Vec<usize> = artifact
        .downbeats()
        .iter()
        .filter_map(|frame| beats.binary_search(frame).ok())
        .collect();
    let Some(beats_per_bar) = most_frequent(
        ordinals
            .windows(2)
            .filter_map(|pair| pair[1].checked_sub(pair[0]))
            .filter(|&gap| gap > 0),
    ) else {
        return Ok(None);
    };
    let bar = u16::try_from(beats_per_bar).map_err(|_| BeatGridError::BarTooLong {
        beats: beats_per_bar,
    })?;
    let phase = most_frequent(ordinals.iter().map(|ordinal| ordinal % beats_per_bar)).unwrap_or(0);
    Ok(Some(
        Meter::new(bar)?.with_downbeat(BeatOrdinal::new(phase.as_())),
    ))
}

/// The value seen most often, the smallest one on a tie.
fn most_frequent(values: impl Iterator<Item = usize>) -> Option<usize> {
    let mut counts = BTreeMap::<usize, usize>::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(value, count)| (count, Reverse(value)))
        .map(|(value, _)| value)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use ::kithara::warp::{
        Beat, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridSnapshot, BeatGridState,
        MapPoint,
    };
    use kithara_test_utils::kithara;

    use super::*;

    const BEAT_FRAMES: u64 = 24_000;

    fn artifact(downbeats: &[u64]) -> BeatArtifact {
        artifact_of(8, downbeats)
    }

    fn artifact_of(beat_count: u64, downbeats: &[u64]) -> BeatArtifact {
        let beats = (0..beat_count)
            .map(|beat| (beat * BEAT_FRAMES, Some(1.0)))
            .collect();
        BeatArtifact::new(
            120.0,
            beats,
            downbeats
                .iter()
                .map(|beat| (beat * BEAT_FRAMES, Some(1.0)))
                .collect(),
        )
    }

    fn axis() -> AssetAxis {
        AssetAxis::new(NonZeroU32::new(48_000).expect("rate"), 8 * BEAT_FRAMES)
    }

    fn meter_at(set: SegmentSet, beat: f64) -> Meter {
        let snapshot = BeatGridSnapshot::segments(
            BeatGridId::allocate().expect("grid id"),
            BeatGridRevision::first(),
            BeatGridState::Complete,
            set,
        )
        .expect("asset snapshot");
        let point = MapPoint::new(snapshot.stamp(), Beat::new(beat).expect("finite beat"));
        match snapshot.meter_at(point) {
            BeatGridQuery::Resolved(estimate) => *estimate.value(),
            other => panic!("meter is resolved inside the grid: {other:?}"),
        }
    }

    #[kithara::test]
    fn every_beat_interval_becomes_one_segment() {
        let set = segment_set(&artifact(&[]), axis()).expect("segment set");

        assert_eq!(set.segments().len(), 7);
    }

    #[kithara::test]
    fn four_beats_per_bar_come_from_the_downbeats() {
        let meter = meter_at(segment_set(&artifact(&[2, 6]), axis()).expect("set"), 3.0);

        assert_eq!(meter.beats_per_bar(), 4);
        assert_eq!(meter.downbeat(), BeatOrdinal::new(2));
    }

    #[kithara::test]
    fn an_irregular_intro_bar_does_not_set_the_meter() {
        let beats = 26;
        let axis = AssetAxis::new(NonZeroU32::new(48_000).expect("rate"), beats * BEAT_FRAMES);
        let set = segment_set(&artifact_of(beats, &[1, 7, 13, 17, 21, 25]), axis).expect("set");
        let meter = meter_at(set, 14.0);

        assert_eq!(meter.beats_per_bar(), 4);
        assert_eq!(i64::from(meter.downbeat()).rem_euclid(4), 1);
    }

    #[kithara::test]
    fn a_downbeat_off_the_beats_is_skipped() {
        let base = artifact(&[0, 4]);
        let downbeats = BeatArtifact::new(
            base.bpm(),
            base.beats().iter().map(|&b| (b, None)).collect(),
            vec![(0, None), (BEAT_FRAMES + 1, None), (4 * BEAT_FRAMES, None)],
        );
        let meter = meter_at(segment_set(&downbeats, axis()).expect("set"), 3.0);

        assert_eq!(meter.beats_per_bar(), 4);
        assert_eq!(meter.downbeat(), BeatOrdinal::new(0));
    }

    #[kithara::test]
    fn one_beat_cannot_form_a_grid() {
        let single = BeatArtifact::new(120.0, vec![(0, None)], Vec::new());

        assert!(matches!(
            segment_set(&single, axis()),
            Err(BeatGridError::TooFewBeats)
        ));
    }
}
