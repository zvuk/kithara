use std::num::NonZeroU32;

use num_traits::cast::ToPrimitive;

use crate::{analysis::TrackAnalysis, waveform::BeatGrid};

/// An invalid musical or source-frame coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum CoordinateError {
    /// The supplied coordinate is `NaN` or infinite.
    #[error("coordinate must be finite")]
    NonFinite,
    /// A source-frame coordinate was below the start of the source.
    #[error("source-frame coordinate must not be negative")]
    NegativeSourceFrame,
    /// An integer source index cannot be converted to the continuous axis exactly.
    #[error("source-frame index {index} cannot be represented exactly")]
    InexactSourceIndex { index: u64 },
}

/// A continuous coordinate in decoded, normalized, host-rate source audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct SourceFrame(f64);

impl SourceFrame {
    /// Creates a finite, non-negative source-frame coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when `value` is not finite or is negative.
    pub fn new(value: f64) -> Result<Self, CoordinateError> {
        if !value.is_finite() {
            return Err(CoordinateError::NonFinite);
        }
        if value < 0.0 {
            return Err(CoordinateError::NegativeSourceFrame);
        }
        Ok(Self(value))
    }
}

impl TryFrom<u64> for SourceFrame {
    type Error = CoordinateError;

    fn try_from(index: u64) -> Result<Self, Self::Error> {
        let value = index
            .to_f64()
            .filter(|value| value.to_u64() == Some(index))
            .ok_or(CoordinateError::InexactSourceIndex { index })?;
        Ok(Self(value))
    }
}

/// A continuous coordinate in one track's analysed beat map.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, derive_more::Into)]
pub struct TrackBeat(f64);

impl TrackBeat {
    /// Creates a finite track-beat coordinate. Negative beats are valid.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinateError`] when `value` is not finite.
    pub fn new(value: f64) -> Result<Self, CoordinateError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(CoordinateError::NonFinite)
        }
    }
}

/// A track analysis cannot provide a valid beat-to-source mapping.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BeatMapError {
    /// The analysis snapshot contains no beat grid.
    #[error("track analysis has no beat grid")]
    Missing,
    /// The analysis snapshot does not define the sample-rate axis of its markers.
    #[error("track analysis does not identify its source sample rate")]
    SourceRateMissing,
    /// Piecewise interpolation cannot be defined from fewer than two markers.
    #[error("beat map requires at least two markers, got {count}")]
    InsufficientMarkers { count: usize },
    /// A beat marker repeats or precedes its predecessor.
    #[error("beat marker {index} is not strictly after its predecessor")]
    NonIncreasingMarker { index: usize },
    /// A beat marker lies beyond the decoded source extent.
    #[error("beat marker {index} at source frame {frame} exceeds source extent {source_frames}")]
    MarkerPastSourceEnd {
        index: usize,
        frame: u64,
        source_frames: u64,
    },
    /// A downbeat does not coincide with an analysed beat marker.
    #[error("downbeat {index} at source frame {frame} is not a beat marker")]
    DownbeatNotOnBeat { index: usize, frame: u64 },
    /// A downbeat repeats or precedes its predecessor.
    #[error("downbeat {index} is not strictly after its predecessor")]
    NonIncreasingDownbeat { index: usize },
    /// A beat marker cannot be represented on the source-frame axis.
    #[error("beat marker {index} has an invalid source coordinate: {source}")]
    SourceCoordinate {
        index: usize,
        #[source]
        source: CoordinateError,
    },
    /// Marker ordinals cannot be represented exactly as continuous beats.
    #[error("beat map contains too many markers for an exact beat coordinate")]
    MarkerCountTooLarge,
    /// The decoded source extent cannot be represented on the host-rate axis.
    #[error(
        "source extent {source_frames} at {source_sample_rate} Hz is out of range at {host_sample_rate} Hz"
    )]
    SourceExtentOutOfRange {
        source_frames: u64,
        source_sample_rate: u32,
        host_sample_rate: u32,
    },
}

/// Immutable piecewise-linear mapping from analysed track beats to host-rate source frames.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(get)]
#[non_exhaustive]
pub struct TrackBeatMap {
    /// Returns the consistently observed number of beats per bar, when
    /// available.
    #[field(get, copy)]
    beats_per_bar: Option<u16>,
    /// Returns analysed downbeats on the track-beat axis.
    #[field(get)]
    downbeats: Vec<TrackBeat>,
    /// Returns the host sample rate defining the source-frame axis.
    #[field(get, copy)]
    host_sample_rate: NonZeroU32,
    /// Returns the exclusive host-rate source bound for the range `0..count`.
    #[field(get, copy)]
    source_frame_count: u64,
    #[field(skip)]
    marker_ordinals: Vec<f64>,
    #[field(skip)]
    source_markers: Vec<SourceFrame>,
    #[field(skip)]
    zero_index: usize,
}

impl TrackBeatMap {
    /// Builds a map and converts its source-rate markers to `host_sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns [`BeatMapError`] when the analysis lacks the required metadata or
    /// its beat markers cannot form a valid mapping.
    pub fn new(
        analysis: &TrackAnalysis,
        host_sample_rate: NonZeroU32,
    ) -> Result<Self, BeatMapError> {
        let grid = analysis.beat().ok_or(BeatMapError::Missing)?;
        let source_rate = analysis
            .source_sample_rate()
            .ok_or(BeatMapError::SourceRateMissing)?;
        Self::build(
            grid,
            analysis.source_frames(),
            source_rate,
            host_sample_rate,
        )
    }

    /// Maps a track beat within the source extent to a source frame.
    ///
    /// Returns `None` outside the source extent.
    #[must_use]
    pub fn source_frame_at(&self, beat: TrackBeat) -> Option<SourceFrame> {
        let zero = self.zero_index.to_f64()?;
        let ordinal = f64::from(beat) + zero;
        let first_ordinal = *self.marker_ordinals.first()?;
        let last_ordinal = *self.marker_ordinals.last()?;
        if ordinal < first_ordinal || ordinal > last_ordinal {
            return None;
        }
        if ordinal == last_ordinal {
            return self.source_markers.last().copied();
        }
        let upper = self
            .marker_ordinals
            .partition_point(|marker| *marker <= ordinal);
        let segment = upper.checked_sub(1)?;
        let start_ordinal = *self.marker_ordinals.get(segment)?;
        let end_ordinal = *self.marker_ordinals.get(segment + 1)?;
        let start = f64::from(*self.source_markers.get(segment)?);
        let end = f64::from(*self.source_markers.get(segment + 1)?);
        let fraction = (ordinal - start_ordinal) / (end_ordinal - start_ordinal);
        SourceFrame::new(start + (end - start) * fraction).ok()
    }

    /// Maps a host-rate source frame within the source extent to a track beat.
    ///
    /// Returns `None` outside the source extent.
    #[must_use]
    pub fn track_beat_at(&self, source: SourceFrame) -> Option<TrackBeat> {
        let first = self.source_markers.first()?;
        let last = self.source_markers.last()?;
        if source < *first || source > *last {
            return None;
        }
        let upper = self
            .source_markers
            .partition_point(|marker| *marker <= source);
        let segment = match upper {
            0 => 0,
            value if value >= self.source_markers.len() => self.source_markers.len() - 2,
            value => value - 1,
        };
        let start = f64::from(self.source_markers[segment]);
        let end = f64::from(self.source_markers[segment + 1]);
        let fraction = (f64::from(source) - start) / (end - start);
        let start_ordinal = self.marker_ordinals[segment];
        let end_ordinal = self.marker_ordinals[segment + 1];
        let ordinal = start_ordinal + (end_ordinal - start_ordinal) * fraction;
        let beat = ordinal - self.zero_index.to_f64()?;
        TrackBeat::new(beat).ok()
    }

    fn build(
        grid: &BeatGrid,
        source_frames: u64,
        source_rate: NonZeroU32,
        host_sample_rate: NonZeroU32,
    ) -> Result<Self, BeatMapError> {
        if grid.beats().len() < 2 {
            return Err(BeatMapError::InsufficientMarkers {
                count: grid.beats().len(),
            });
        }
        let source_frame_count =
            scale_source_frame_count(source_frames, source_rate, host_sample_rate)?;

        let scale = f64::from(host_sample_rate.get()) / f64::from(source_rate.get());
        let source_extent = SourceFrame::try_from(source_frames)
            .and_then(|frame| SourceFrame::new(f64::from(frame) * scale))
            .map_err(|_| BeatMapError::SourceExtentOutOfRange {
                source_frames,
                source_sample_rate: source_rate.get(),
                host_sample_rate: host_sample_rate.get(),
            })?;
        let mut marker_ordinals = Vec::with_capacity(grid.beats().len() + 2);
        let mut source_markers = Vec::with_capacity(grid.beats().len() + 2);
        for (index, &frame) in grid.beats().iter().enumerate() {
            if index > 0 && frame <= grid.beats()[index - 1] {
                return Err(BeatMapError::NonIncreasingMarker { index });
            }
            if frame > source_frames {
                return Err(BeatMapError::MarkerPastSourceEnd {
                    index,
                    frame,
                    source_frames,
                });
            }
            let source = SourceFrame::try_from(frame)
                .and_then(|frame| SourceFrame::new(f64::from(frame) * scale))
                .map_err(|source| BeatMapError::SourceCoordinate { index, source })?;
            marker_ordinals.push(index.to_f64().ok_or(BeatMapError::MarkerCountTooLarge)?);
            source_markers.push(source);
        }

        let mut downbeat_indices = Vec::with_capacity(grid.downbeats().len());
        for (index, &frame) in grid.downbeats().iter().enumerate() {
            let beat_index = grid
                .beats()
                .binary_search(&frame)
                .map_err(|_| BeatMapError::DownbeatNotOnBeat { index, frame })?;
            if downbeat_indices
                .last()
                .is_some_and(|previous| beat_index <= *previous)
            {
                return Err(BeatMapError::NonIncreasingDownbeat { index });
            }
            downbeat_indices.push(beat_index);
        }

        let zero_index = downbeat_indices.first().copied().unwrap_or(0);
        let downbeats = downbeat_indices
            .iter()
            .map(|index| track_beat_for_index(*index, zero_index))
            .collect::<Result<Vec<_>, _>>()?;
        let beats_per_bar = consistent_meter(&downbeat_indices);

        if grid.beats()[0] > 0 {
            let first = f64::from(source_markers[0]);
            let slope = f64::from(source_markers[1]) - first;
            marker_ordinals.insert(0, -first / slope);
            source_markers.insert(0, SourceFrame::default());
        }
        if grid.beats()[grid.beats().len() - 1] < source_frames {
            let last = source_markers.len() - 1;
            let slope = f64::from(source_markers[last]) - f64::from(source_markers[last - 1]);
            let ordinal = marker_ordinals[last]
                + (f64::from(source_extent) - f64::from(source_markers[last])) / slope;
            marker_ordinals.push(ordinal);
            source_markers.push(source_extent);
        }

        Ok(Self {
            beats_per_bar,
            downbeats,
            host_sample_rate,
            source_frame_count,
            marker_ordinals,
            source_markers,
            zero_index,
        })
    }
}

fn scale_source_frame_count(
    source_frames: u64,
    source_sample_rate: NonZeroU32,
    host_sample_rate: NonZeroU32,
) -> Result<u64, BeatMapError> {
    let out_of_range = || BeatMapError::SourceExtentOutOfRange {
        source_frames,
        source_sample_rate: source_sample_rate.get(),
        host_sample_rate: host_sample_rate.get(),
    };
    let numerator = u128::from(source_frames)
        .checked_mul(u128::from(host_sample_rate.get()))
        .and_then(|value| value.checked_add(u128::from(source_sample_rate.get() / 2)))
        .ok_or_else(out_of_range)?;
    let scaled = numerator / u128::from(source_sample_rate.get());
    u64::try_from(scaled).map_err(|_| out_of_range())
}

fn track_beat_for_index(index: usize, zero_index: usize) -> Result<TrackBeat, BeatMapError> {
    let index = index.to_f64().ok_or(BeatMapError::MarkerCountTooLarge)?;
    let zero = zero_index
        .to_f64()
        .ok_or(BeatMapError::MarkerCountTooLarge)?;
    TrackBeat::new(index - zero).map_err(|_| BeatMapError::MarkerCountTooLarge)
}

fn consistent_meter(downbeats: &[usize]) -> Option<u16> {
    let first = downbeats.windows(2).next().map(|pair| pair[1] - pair[0])?;
    if first == 0 || downbeats.windows(2).any(|pair| pair[1] - pair[0] != first) {
        return None;
    }
    u16::try_from(first).ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;
    use num_traits::cast::ToPrimitive;

    use super::{BeatMapError, CoordinateError, SourceFrame, TrackBeat, TrackBeatMap};
    use crate::{analysis::TrackAnalysis, waveform::BeatGrid};

    fn sample_rate(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("invariant: test sample rate is non-zero")
    }

    fn grid(beats: Vec<u64>) -> BeatGrid {
        let downbeats = beats.first().copied().into_iter().collect();
        BeatGrid::new(120.0, beats, downbeats, Vec::new())
    }

    fn analysis_with_grid(beats: Vec<u64>, source_frames: u64, source_rate: u32) -> TrackAnalysis {
        TrackAnalysis::with_source_rate(
            Some(grid(beats)),
            None,
            source_frames,
            sample_rate(source_rate),
        )
    }

    #[kithara::test]
    fn rejects_missing_beat_grid() {
        let analysis = TrackAnalysis::with_source_rate(None, None, 1_000, sample_rate(48_000));

        assert_eq!(
            TrackBeatMap::new(&analysis, sample_rate(48_000)),
            Err(BeatMapError::Missing)
        );
    }

    #[kithara::test]
    fn rejects_fewer_than_two_markers() {
        let analysis = analysis_with_grid(vec![0], 1_000, 48_000);

        assert_eq!(
            TrackBeatMap::new(&analysis, sample_rate(48_000)),
            Err(BeatMapError::InsufficientMarkers { count: 1 })
        );
    }

    #[kithara::test]
    fn rejects_non_increasing_markers() {
        let analysis = analysis_with_grid(vec![0, 480, 480], 1_000, 48_000);

        assert_eq!(
            TrackBeatMap::new(&analysis, sample_rate(48_000)),
            Err(BeatMapError::NonIncreasingMarker { index: 2 })
        );
    }

    #[kithara::test]
    fn rejects_missing_source_sample_rate() {
        let analysis = TrackAnalysis::new(Some(grid(vec![0, 480])), None, 1_000);

        assert_eq!(
            TrackBeatMap::new(&analysis, sample_rate(48_000)),
            Err(BeatMapError::SourceRateMissing)
        );
    }

    #[kithara::test]
    fn same_rate_markers_round_trip_exactly() {
        let markers = vec![120, 480, 960];
        let analysis = analysis_with_grid(markers.clone(), 1_200, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid same-rate beat map");

        for (index, marker) in markers.into_iter().enumerate() {
            let beat = TrackBeat::new(
                index
                    .to_f64()
                    .expect("invariant: small marker ordinal is exactly representable"),
            )
            .expect("invariant: marker ordinal is finite");
            let source = map
                .source_frame_at(beat)
                .expect("invariant: marker beat is in the analysed domain");
            let expected = SourceFrame::try_from(marker)
                .expect("invariant: small source marker is exactly representable");

            assert_eq!(source, expected);
            assert_eq!(map.track_beat_at(source), Some(beat));
        }
    }

    #[kithara::test]
    fn markers_scale_exactly_from_44100_to_48000() {
        let markers = vec![0, 11_025, 22_050, 44_100];
        let expected = [0.0, 12_000.0, 24_000.0, 48_000.0];
        let analysis = analysis_with_grid(markers, 88_200, 44_100);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid resampled beat map");

        for (index, expected) in expected.into_iter().enumerate() {
            let beat = TrackBeat::new(
                index
                    .to_f64()
                    .expect("invariant: small marker ordinal is exactly representable"),
            )
            .expect("invariant: marker ordinal is finite");
            let source = map
                .source_frame_at(beat)
                .expect("invariant: marker beat is in the analysed domain");

            // The 160/147 ratio is not representable in f64; markers land within
            // sub-nanosample rounding of the exact rational.
            assert!(
                (f64::from(source) - expected).abs() <= 1e-9,
                "scaled marker: expected {expected}, got {}",
                f64::from(source)
            );
        }
        assert_eq!(map.source_frame_count(), 96_000);
    }

    #[kithara::test]
    fn a_beat_before_the_seeded_start_resolves_to_no_source_frame() {
        let analysis = analysis_with_grid(vec![100, 200, 300], 400, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid bounded beat map");
        let before = TrackBeat::new(-1.5).expect("invariant: negative track beats are valid");

        assert_eq!(map.source_frame_at(before), None);
    }

    #[kithara::test]
    fn a_beat_past_the_seeded_end_resolves_to_no_source_frame() {
        let analysis = analysis_with_grid(vec![100, 200, 300], 400, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid bounded beat map");
        let after = TrackBeat::new(3.5).expect("invariant: finite track beat is valid");

        assert_eq!(map.source_frame_at(after), None);
    }

    #[kithara::test]
    fn a_position_before_the_first_detected_beat_resolves_to_a_track_beat() {
        let analysis = analysis_with_grid(vec![100, 200, 300], 400, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid beat map with an intro");
        let source = SourceFrame::new(50.0).expect("invariant: intro position is valid");
        let expected = TrackBeat::new(-0.5).expect("invariant: expected beat is finite");

        assert_eq!(map.track_beat_at(source), Some(expected));
    }

    #[kithara::test]
    fn a_position_after_the_last_detected_beat_resolves_to_a_track_beat() {
        let analysis = analysis_with_grid(vec![100, 200, 300], 400, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid beat map with an outro");
        let source = SourceFrame::new(350.0).expect("invariant: outro position is valid");
        let expected = TrackBeat::new(2.5).expect("invariant: expected beat is finite");

        assert_eq!(map.track_beat_at(source), Some(expected));
    }

    #[kithara::test]
    fn the_beat_ordinal_of_every_detected_beat_is_unchanged_by_the_seeding() {
        let beats = vec![100, 200, 300, 400];
        let grid = BeatGrid::new(120.0, beats.clone(), vec![300], Vec::new());
        let analysis = TrackAnalysis::with_source_rate(Some(grid), None, 500, sample_rate(48_000));
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid beat map with seeded edges");

        for (frame, expected) in beats.into_iter().zip([-2.0, -1.0, 0.0, 1.0]) {
            let source = SourceFrame::try_from(frame)
                .expect("invariant: detected beat frame is exactly representable");
            let expected =
                TrackBeat::new(expected).expect("invariant: expected beat ordinal is finite");

            assert_eq!(map.track_beat_at(source), Some(expected));
        }
    }

    #[kithara::test]
    fn a_position_outside_the_source_extent_still_resolves_to_nothing() {
        let analysis = analysis_with_grid(vec![100, 200, 300], 400, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid bounded beat map");
        let source = SourceFrame::new(401.0).expect("invariant: source position is valid");

        assert_eq!(map.track_beat_at(source), None);
    }

    #[kithara::test]
    fn a_track_whose_first_detected_beat_is_already_at_frame_zero_gains_no_duplicate_marker() {
        let analysis = analysis_with_grid(vec![0, 100, 200], 300, 48_000);
        let map = TrackBeatMap::new(&analysis, sample_rate(48_000))
            .expect("invariant: valid beat map starting at frame zero");
        let zero = SourceFrame::default();

        assert_eq!(
            map.source_markers
                .iter()
                .filter(|marker| **marker == zero)
                .count(),
            1
        );
    }

    #[kithara::test]
    fn coordinates_reject_non_finite_and_negative_values() {
        assert_eq!(TrackBeat::new(f64::NAN), Err(CoordinateError::NonFinite));
        assert_eq!(
            TrackBeat::new(f64::INFINITY),
            Err(CoordinateError::NonFinite)
        );
        assert_eq!(SourceFrame::new(f64::NAN), Err(CoordinateError::NonFinite));
        assert_eq!(
            SourceFrame::new(f64::NEG_INFINITY),
            Err(CoordinateError::NonFinite)
        );
        assert_eq!(
            SourceFrame::new(-1.0),
            Err(CoordinateError::NegativeSourceFrame)
        );
        assert_eq!(
            f64::from(
                TrackBeat::new(-1.0)
                    .expect("invariant: negative track beats are part of the coordinate domain")
            ),
            -1.0
        );
    }
}
