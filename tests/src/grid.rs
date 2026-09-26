//! Expresses a fixture's [`BeatArtifact`] as the warp [`SegmentSet`] a deck
//! publishes for a track, and reads the beat grid the fixture build analysed
//! for an audio fixture. Fixture support: the product publishes
//! grids from its own analysis bridge, not through this module.

use std::{cmp::Reverse, collections::BTreeMap};

use kithara::{
    analysis::{AnalysisFile, AnalysisFingerprint, BeatArtifact, BeatGridModel, GridBeat},
    warp::{
        AssetAxis, AssetFrame, BeatEvidence, BeatMarker, BeatOrdinal, FrameUncertainty, MapAxis,
        MapCoordinateError, MapSegment, Meter, MeterError, MeterFacts, SegmentError, SegmentFacts,
        SegmentSet,
    },
};
use kithara_test_fixtures::assets::by_name;
use num_traits::cast::AsPrimitive;

/// The fingerprint the fixture build writes every analysis file under.
const ANALYSIS_FINGERPRINT: &str = "rhythm-fixture:v1";

/// The beat grid the fixture build analysed for the audio fixture `track`,
/// from its derived `analysis_<track>` asset. A test reads it instead of
/// analysing the track again.
///
/// # Panics
///
/// When `track` has no analysis, or its analysis is unreadable or states no
/// grid.
#[must_use]
pub fn analysed_grid(track: &str) -> BeatGridModel {
    let name = format!("analysis_{track}");
    let asset = by_name(&name).unwrap_or_else(|| panic!("analysis `{name}` is not registered"));
    let file = AnalysisFile::parse(
        asset.bytes(),
        &AnalysisFingerprint::new(Some(ANALYSIS_FINGERPRINT), None),
    )
    .unwrap_or_else(|error| panic!("decode `{name}`: {error:?}"));
    BeatGridModel::try_from(file.latest().analysis())
        .unwrap_or_else(|error| panic!("`{name}` carries no usable beat grid: {error:?}"))
}

/// Where a test starts a track. A track with an analysed grid opens where the
/// music does, on a beat the grid names; a track without one opens at a
/// second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Start {
    /// Beat `beat` of bar `bar`, bars counted from the grid's first downbeat:
    /// beat 0 is the bar's downbeat, a later one enters on a weak beat.
    Bar { bar: usize, beat: i64 },
    /// Analysed beat `ordinal`, for a grid that states no bars.
    Beat(i64),
    /// A second of a track that has no analysed grid.
    Seconds(f64),
}

impl Start {
    /// The downbeat of bar `bar`.
    #[must_use]
    pub const fn bar(bar: usize) -> Self {
        Self::Bar { bar, beat: 0 }
    }

    /// The second this start opens the track at; `grid` is the track's
    /// analysed grid, if it has one.
    ///
    /// # Panics
    ///
    /// When a start on the grid meets a track without one, or [`Self::beat`]
    /// panics.
    #[must_use]
    pub fn seconds(self, grid: Option<&BeatGridModel>) -> f64 {
        match self {
            Self::Seconds(seconds) => seconds,
            on_grid => {
                let grid = grid.unwrap_or_else(|| panic!("{on_grid:?} needs a track with a grid"));
                on_grid.beat(grid).at
            }
        }
    }

    /// The analysed beat this start names in `grid`.
    ///
    /// # Panics
    ///
    /// When `grid` names no such beat: a bar past its downbeats, bars on a
    /// grid that states none, or a start at a second.
    #[must_use]
    pub fn beat(self, grid: &BeatGridModel) -> GridBeat {
        let raw = grid.as_raw();
        let ordinal = match self {
            Self::Bar { bar, beat } => {
                let downbeat = raw.downbeats.get(bar).unwrap_or_else(|| {
                    panic!(
                        "bar {bar} is past the {} downbeats the grid names",
                        raw.downbeats.len()
                    )
                });
                downbeat.beat_ordinal + beat
            }
            Self::Beat(ordinal) => ordinal,
            Self::Seconds(seconds) => panic!("a start at {seconds} s names no beat"),
        };
        *raw.beats
            .iter()
            .find(|beat| beat.ordinal == ordinal)
            .unwrap_or_else(|| panic!("{self:?} names beat {ordinal}, which the grid does not"))
    }
}

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
        AssetExtent, Beat, BeatGridId, BeatGridQuery, BeatGridRevision, BeatGridSnapshot,
        BeatGridState, MapPoint,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::consts;

    fn artifact(downbeats: &[u64]) -> BeatArtifact {
        artifact_of(8, downbeats)
    }

    fn artifact_of(beat_count: u64, downbeats: &[u64]) -> BeatArtifact {
        let beats = (0..beat_count)
            .map(|beat| (beat * consts::BEAT_FRAMES, Some(1.0)))
            .collect();
        BeatArtifact::new(
            120.0,
            beats,
            downbeats
                .iter()
                .map(|beat| (beat * consts::BEAT_FRAMES, Some(1.0)))
                .collect(),
        )
    }

    fn axis() -> AssetAxis {
        AssetAxis::new(
            NonZeroU32::new(48_000).expect("rate"),
            AssetExtent::Bounded(8 * consts::BEAT_FRAMES),
        )
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
        let axis = AssetAxis::new(
            NonZeroU32::new(48_000).expect("rate"),
            AssetExtent::Bounded(beats * consts::BEAT_FRAMES),
        );
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
            vec![
                (0, None),
                (consts::BEAT_FRAMES + 1, None),
                (4 * consts::BEAT_FRAMES, None),
            ],
        );
        let meter = meter_at(segment_set(&downbeats, axis()).expect("set"), 3.0);

        assert_eq!(meter.beats_per_bar(), 4);
        assert_eq!(meter.downbeat(), BeatOrdinal::new(0));
    }

    #[kithara::test]
    fn a_start_names_its_beat_from_the_bar_downbeats() {
        let beat = |ordinal: i64| ::kithara::analysis::GridBeat {
            at: f64_of(ordinal) * 0.5,
            ordinal,
            confidence: None,
        };
        let downbeat = |ordinal: i64| ::kithara::analysis::GridDownbeat {
            at: f64_of(ordinal) * 0.5,
            beat_ordinal: ordinal,
            confidence: None,
        };
        let grid = BeatGridModel::try_from(::kithara::analysis::RawBeatGrid {
            schema_version: ::kithara::analysis::GRID_SCHEMA_VERSION,
            model_id: "bars".to_owned(),
            revision: 1,
            state: ::kithara::analysis::BeatGridState::Final,
            duration: None,
            bpm: 120.0,
            beats: (0..12).map(beat).collect(),
            downbeats: [2, 6, 10].into_iter().map(downbeat).collect(),
            meter: None,
        })
        .expect("downbeats on beats form a grid");

        assert_eq!(Start::bar(0).beat(&grid).ordinal, 2);
        assert_eq!(Start::bar(2).beat(&grid).at, 5.0);
        assert_eq!(Start::Bar { bar: 1, beat: 1 }.beat(&grid).ordinal, 7);
        assert_eq!(Start::Beat(3).beat(&grid).ordinal, 3);
        assert_eq!(Start::bar(1).seconds(Some(&grid)), 3.0);
        assert_eq!(Start::Seconds(1.25).seconds(None), 1.25);
    }

    fn f64_of(ordinal: i64) -> f64 {
        ordinal.as_()
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
