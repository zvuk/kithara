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
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
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

    /// Returns the continuous source-frame coordinate.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
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
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
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

    /// Returns the continuous track-beat coordinate.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
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
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TrackBeatMap {
    beats_per_bar: Option<u16>,
    downbeats: Vec<TrackBeat>,
    host_sample_rate: NonZeroU32,
    source_frame_count: u64,
    source_markers: Vec<SourceFrame>,
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

    /// Returns the consistently observed number of beats per bar, when available.
    #[must_use]
    pub fn beats_per_bar(&self) -> Option<u16> {
        self.beats_per_bar
    }

    /// Returns analysed downbeats on the track-beat axis.
    #[must_use]
    pub fn downbeats(&self) -> &[TrackBeat] {
        &self.downbeats
    }

    /// Returns the host sample rate defining the source-frame axis.
    #[must_use]
    pub fn host_sample_rate(&self) -> NonZeroU32 {
        self.host_sample_rate
    }

    /// Returns the exclusive host-rate source bound for the range `0..count`.
    #[must_use]
    pub const fn source_frame_count(&self) -> u64 {
        self.source_frame_count
    }

    /// Maps a track beat within the analysed marker domain to a source frame.
    ///
    /// Returns `None` before the first marker or after the last marker.
    #[must_use]
    pub fn source_frame_at(&self, beat: TrackBeat) -> Option<SourceFrame> {
        let zero = self.zero_index.to_f64()?;
        let ordinal = beat.get() + zero;
        let last = self.source_markers.len().checked_sub(1)?;
        let last_f64 = last.to_f64()?;
        if ordinal < 0.0 || ordinal > last_f64 {
            return None;
        }
        if ordinal == last_f64 {
            return self.source_markers.get(last).copied();
        }
        let segment = ordinal.floor().to_usize()?;
        let start = self.source_markers.get(segment)?.get();
        let end = self.source_markers.get(segment + 1)?.get();
        let fraction = ordinal - segment.to_f64()?;
        SourceFrame::new(start + (end - start) * fraction).ok()
    }

    /// Maps a host-rate source frame within the analysed marker domain to a track beat.
    ///
    /// Returns `None` before the first marker or after the last marker.
    #[must_use]
    pub fn track_beat_at(&self, source: SourceFrame) -> Option<TrackBeat> {
        let first = self.source_markers.first()?;
        let last = self.source_markers.last()?;
        if source < *first || source > *last {
            return None;
        }
        let upper = self
            .source_markers
            .partition_point(|marker| marker.get() <= source.get());
        let segment = match upper {
            0 => 0,
            value if value >= self.source_markers.len() => self.source_markers.len() - 2,
            value => value - 1,
        };
        let start = self.source_markers[segment].get();
        let end = self.source_markers[segment + 1].get();
        let fraction = (source.get() - start) / (end - start);
        let beat = segment.to_f64()? - self.zero_index.to_f64()? + fraction;
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
        let last_index = grid.beats().len() - 1;
        track_beat_for_index(last_index, 0)?;
        let source_frame_count =
            scale_source_frame_count(source_frames, source_rate, host_sample_rate)?;

        let scale = f64::from(host_sample_rate.get()) / f64::from(source_rate.get());
        let mut source_markers = Vec::with_capacity(grid.beats().len());
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
                .and_then(|frame| SourceFrame::new(frame.get() * scale))
                .map_err(|source| BeatMapError::SourceCoordinate { index, source })?;
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

        Ok(Self {
            beats_per_bar,
            downbeats,
            host_sample_rate,
            source_frame_count,
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
