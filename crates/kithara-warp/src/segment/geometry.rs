use std::{cmp::Ordering, ops::RangeInclusive};

use kithara_platform::sync::Arc;
use num_traits::ToPrimitive;

use super::{BeatEvidence, BeatMarker, BeatsPerMinute, Meter, MeterFacts, SegmentFacts};
use crate::{AssetFrame, AxisKind, Beat, FrameUncertainty, MapAxis, MapPosition, SessionFrame};

const SECONDS_PER_MINUTE: f64 = 60.0;

/// Which segment endpoint failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SegmentEndpoint {
    /// The segment start.
    Start,
    /// The segment end.
    End,
}

/// A segment or segment collection is not a valid musical topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SegmentError {
    /// A timestamp lacks the ordinal needed to preserve musical distance.
    #[error("{endpoint:?} marker has no explicit musical ordinal")]
    MissingOrdinal { endpoint: SegmentEndpoint },
    /// Segment endpoints use different coordinate axes.
    #[error("segment endpoints must use the same coordinate axis")]
    MixedAxes,
    /// A musical ordinal cannot be represented exactly by the query axis.
    #[error("{endpoint:?} marker ordinal cannot be represented exactly")]
    InexactOrdinal { endpoint: SegmentEndpoint },
    /// The segment position span does not move forward.
    #[error("segment end position must follow its start position")]
    NonIncreasingPosition,
    /// The segment beat span does not move forward.
    #[error("segment end ordinal must follow its start ordinal")]
    NonIncreasingBeat,
    /// A segment does not match the snapshot axis.
    #[error("segment {index} does not match the snapshot coordinate axis")]
    AxisMismatch { index: usize },
    /// A segment ends beyond the bounded asset extent.
    #[error("segment {index} is outside the bounded asset extent")]
    OutsideExtent { index: usize },
    /// The segment slope cannot produce a finite positive tempo.
    #[error("segment {index} has an unrepresentable tempo")]
    InvalidTempo { index: usize },
    /// A segment is not ordered after its predecessor.
    #[error("segment {index} begins before its predecessor")]
    OutOfOrder { index: usize },
    /// A segment overlaps its predecessor.
    #[error("segment {index} overlaps its predecessor")]
    Overlap { index: usize },
    /// Musical coordinates reverse between adjacent segments.
    #[error("segment {index} reverses the musical coordinate axis")]
    BeatOrder { index: usize },
    /// Adjacent segments touch in only one of the two coordinate spaces.
    #[error("segment {index} has a non-invertible boundary with its predecessor")]
    NonInvertibleBoundary { index: usize },
}

/// One validated affine relation between grid positions and musical beats.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct MapSegment {
    #[field(get, vis = "pub(crate)", copy)]
    end_beat: Beat,
    #[field(get, vis = "pub(crate)", copy)]
    start_beat: Beat,
    end_evidence: BeatEvidence,
    start_evidence: BeatEvidence,
    end_uncertainty: FrameUncertainty,
    start_uncertainty: FrameUncertainty,
    #[field(get, vis = "pub(crate)", copy)]
    end_position: MapPosition,
    #[field(get, vis = "pub(crate)", copy)]
    start_position: MapPosition,
    facts: SegmentFacts,
}

impl MapSegment {
    /// Validates one position-to-beat relation between two sparse markers.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentError`] when the markers lack musical ordinals, mix
    /// axes, or do not advance in both coordinate spaces.
    pub fn new(
        start: BeatMarker,
        end: BeatMarker,
        facts: SegmentFacts,
    ) -> Result<Self, SegmentError> {
        let start_ordinal = start.ordinal.ok_or(SegmentError::MissingOrdinal {
            endpoint: SegmentEndpoint::Start,
        })?;
        let end_ordinal = end.ordinal.ok_or(SegmentError::MissingOrdinal {
            endpoint: SegmentEndpoint::End,
        })?;
        if start.position.kind() != end.position.kind() {
            return Err(SegmentError::MixedAxes);
        }
        if start.position.partial_cmp(&end.position) != Some(Ordering::Less) {
            return Err(SegmentError::NonIncreasingPosition);
        }
        if start_ordinal >= end_ordinal {
            return Err(SegmentError::NonIncreasingBeat);
        }
        let start_beat =
            Beat::try_from(start_ordinal).map_err(|_| SegmentError::InexactOrdinal {
                endpoint: SegmentEndpoint::Start,
            })?;
        let end_beat = Beat::try_from(end_ordinal).map_err(|_| SegmentError::InexactOrdinal {
            endpoint: SegmentEndpoint::End,
        })?;
        Ok(Self {
            start_beat,
            end_beat,
            facts,
            start_position: start.position,
            end_position: end.position,
            start_evidence: start.evidence,
            end_evidence: end.evidence,
            start_uncertainty: start.uncertainty,
            end_uncertainty: end.uncertainty,
        })
    }

    /// Extends a neighbouring relation over a span no marker describes.
    ///
    /// The beats are computed from the neighbour's own spacing rather than
    /// read off a marker, so they are continuous where a marker ordinal is
    /// integral: an unmarked head rarely begins a whole beat before the first
    /// observed one.
    fn extrapolated(
        start: (MapPosition, Beat),
        end: (MapPosition, Beat),
        uncertainty: FrameUncertainty,
        meter: Option<MeterFacts>,
    ) -> Option<Self> {
        let (start_position, start_beat) = start;
        let (end_position, end_beat) = end;
        if start_position.partial_cmp(&end_position) != Some(Ordering::Less)
            || start_beat >= end_beat
        {
            return None;
        }
        let meter = meter.map(|meter| {
            let (meter, _, uncertainty) = meter.into_parts();
            MeterFacts::new(meter, BeatEvidence::Extrapolated, uncertainty)
        });
        Some(Self {
            start_beat,
            end_beat,
            facts: SegmentFacts::new(BeatEvidence::Extrapolated, uncertainty, meter),
            start_position,
            end_position,
            start_evidence: BeatEvidence::Extrapolated,
            end_evidence: BeatEvidence::Extrapolated,
            start_uncertainty: uncertainty,
            end_uncertainty: uncertainty,
        })
    }

    pub(crate) fn beat_at(
        &self,
        position: MapPosition,
    ) -> Option<(Beat, BeatEvidence, FrameUncertainty)> {
        if !self.contains_position(position) {
            return None;
        }
        if position == self.start_position {
            return Some((self.start_beat, self.start_evidence, self.start_uncertainty));
        }
        if position == self.end_position {
            return Some((self.end_beat, self.end_evidence, self.end_uncertainty));
        }
        let start = f64::try_from(self.start_position).ok()?;
        let end = f64::try_from(self.end_position).ok()?;
        let fraction = (f64::try_from(position).ok()? - start) / (end - start);
        let beat = (f64::from(self.end_beat) - f64::from(self.start_beat))
            .mul_add(fraction, f64::from(self.start_beat));
        Beat::new(beat)
            .ok()
            .map(|value| (value, self.facts.evidence, self.facts.uncertainty))
    }

    pub(crate) fn contains_beat(&self, beat: Beat) -> bool {
        beat >= self.start_beat && beat <= self.end_beat
    }

    pub(crate) fn contains_position(&self, position: MapPosition) -> bool {
        position.kind() == self.kind()
            && position >= self.start_position
            && position <= self.end_position
    }

    pub(crate) const fn kind(&self) -> AxisKind {
        self.start_position.kind()
    }

    pub(crate) fn meter_at(&self, beat: Beat) -> Option<(Meter, BeatEvidence, FrameUncertainty)> {
        if !self.contains_beat(beat) {
            return None;
        }
        let facts = self.facts.meter?;
        Some((facts.meter, facts.evidence, facts.uncertainty))
    }

    pub(crate) fn position_at(
        &self,
        beat: Beat,
    ) -> Option<(MapPosition, BeatEvidence, FrameUncertainty)> {
        if !self.contains_beat(beat) {
            return None;
        }
        if beat == self.start_beat {
            return Some((
                self.start_position,
                self.start_evidence,
                self.start_uncertainty,
            ));
        }
        if beat == self.end_beat {
            return Some((self.end_position, self.end_evidence, self.end_uncertainty));
        }
        let start = f64::from(self.start_beat);
        let end = f64::from(self.end_beat);
        let fraction = (f64::from(beat) - start) / (end - start);
        let start_position = f64::try_from(self.start_position).ok()?;
        let position = (f64::try_from(self.end_position).ok()? - start_position)
            .mul_add(fraction, start_position);
        MapPosition::on_axis(self.kind(), position)
            .map(|value| (value, self.facts.evidence, self.facts.uncertainty))
    }

    /// Returns the inclusive position region represented by this segment.
    #[must_use]
    pub const fn region(&self) -> MapRegion {
        MapRegion {
            start: self.start_position,
            end: self.end_position,
        }
    }

    fn frames_per_beat(&self) -> Option<f64> {
        let frames =
            f64::try_from(self.end_position).ok()? - f64::try_from(self.start_position).ok()?;
        let beats = f64::from(self.end_beat) - f64::from(self.start_beat);
        (beats > 0.0 && frames > 0.0).then_some(frames / beats)
    }

    fn meter_facts(&self) -> Option<MeterFacts> {
        self.facts.meter
    }

    pub(super) fn tempo(&self, axis: MapAxis) -> Option<BeatsPerMinute> {
        let bpm =
            f64::from(axis.sample_rate().get()) * SECONDS_PER_MINUTE / self.frames_per_beat()?;
        BeatsPerMinute::try_from(bpm).ok()
    }

    pub(crate) fn tempo_at(
        &self,
        axis: MapAxis,
        position: MapPosition,
    ) -> Option<(BeatsPerMinute, BeatEvidence, FrameUncertainty)> {
        if !self.contains_position(position) {
            return None;
        }
        self.tempo(axis)
            .map(|tempo| (tempo, self.facts.evidence, self.facts.uncertainty))
    }
}

/// An inclusive grid-native position region.
#[derive(Clone, Copy, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
#[non_exhaustive]
pub struct MapRegion {
    /// Returns the last position in the region.
    #[field(get, copy)]
    end: MapPosition,
    /// Returns the first position in the region.
    #[field(get, copy)]
    start: MapPosition,
}

/// An inclusive native-position region is not ordered on one coordinate axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MapRegionError {
    /// Region endpoints use different coordinate axes.
    #[error("region endpoints must use the same coordinate axis")]
    MixedAxes,
    /// The end precedes the start on their shared coordinate axis.
    #[error("region end must not precede its start")]
    Reversed,
}

impl MapRegion {
    /// Creates an inclusive native-position region from trusted ordered facts.
    #[must_use]
    pub(crate) const fn between(start: MapPosition, end: MapPosition) -> Self {
        Self { end, start }
    }

    /// Creates a region containing one native position.
    #[must_use]
    pub const fn point(position: MapPosition) -> Self {
        Self {
            start: position,
            end: position,
        }
    }
}

impl TryFrom<RangeInclusive<MapPosition>> for MapRegion {
    type Error = MapRegionError;

    fn try_from(range: RangeInclusive<MapPosition>) -> Result<Self, Self::Error> {
        let (start, end) = range.into_inner();
        if start.kind() != end.kind() {
            return Err(MapRegionError::MixedAxes);
        }
        if start > end {
            return Err(MapRegionError::Reversed);
        }
        Ok(Self { end, start })
    }
}

/// An immutable ordered collection of non-overlapping grid segments.
///
/// When two segments touch in both coordinate spaces, their shared seam belongs
/// to the following segment. Forward and inverse queries therefore select the
/// same meter, evidence, and uncertainty at a relation change.
#[derive(Clone, Debug, PartialEq, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct SegmentSet {
    /// Returns all validated segments in coordinate order.
    #[field(get)]
    segments: Arc<[MapSegment]>,
    #[field(get, vis = "pub(crate)", copy)]
    axis: MapAxis,
}

impl SegmentSet {
    /// Covers `axis` with the given segments and validates the result.
    ///
    /// Every span no segment describes is extrapolated from its neighbour
    /// before validation, so the set the caller receives answers over the whole
    /// axis and every segment in it — given or synthesised — has passed the
    /// same checks.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentError`] for an axis or extent mismatch, unrepresentable
    /// tempo, ordering error, overlap, reversed musical coordinates, a seam
    /// that touches in only one coordinate space, or a span whose neighbour
    /// carries no spacing to extrapolate from.
    pub fn new(axis: MapAxis, segments: Vec<MapSegment>) -> Result<Self, SegmentError> {
        let segments = extended(axis, segments)?;
        for (index, segment) in segments.iter().enumerate() {
            if segment.kind() != axis.kind() {
                return Err(SegmentError::AxisMismatch { index });
            }
            if let MapAxis::Asset(asset) = axis {
                let inside = match segment.end_position() {
                    MapPosition::Asset(frame) => asset.contains_or_eof(frame),
                    MapPosition::Session(_) => false,
                };
                if !inside {
                    return Err(SegmentError::OutsideExtent { index });
                }
            }
            if segment.tempo(axis).is_none() {
                return Err(SegmentError::InvalidTempo { index });
            }
            let Some(previous) = index.checked_sub(1).and_then(|value| segments.get(value)) else {
                continue;
            };
            match segment
                .start_position()
                .partial_cmp(&previous.start_position())
            {
                Some(Ordering::Less) | None => return Err(SegmentError::OutOfOrder { index }),
                _ => {}
            }
            if segment.start_position() < previous.end_position() {
                return Err(SegmentError::Overlap { index });
            }
            if segment.start_beat() < previous.end_beat() {
                return Err(SegmentError::BeatOrder { index });
            }
            let position_touches = segment.start_position() == previous.end_position();
            let beat_touches = segment.start_beat() == previous.end_beat();
            if position_touches != beat_touches {
                return Err(SegmentError::NonInvertibleBoundary { index });
            }
        }
        Ok(Self {
            axis,
            segments: segments.into(),
        })
    }

    pub(crate) fn by_beat(&self, beat: Beat) -> Option<&MapSegment> {
        let upper = self
            .segments
            .partition_point(|segment| segment.start_beat() <= beat);
        upper
            .checked_sub(1)
            .and_then(|index| self.segments.get(index))
            .filter(|segment| segment.contains_beat(beat))
    }

    pub(crate) fn by_position(&self, position: MapPosition) -> Option<&MapSegment> {
        let upper = self
            .segments
            .partition_point(|segment| segment.start_position() <= position);
        upper
            .checked_sub(1)
            .and_then(|index| self.segments.get(index))
            .filter(|segment| segment.contains_position(position))
    }

    pub(crate) fn beat_at_or_next(
        &self,
        position: MapPosition,
    ) -> Option<(Beat, BeatEvidence, FrameUncertainty)> {
        let segment = self.segments.get(
            self.segments
                .partition_point(|segment| segment.end_position() < position),
        )?;
        if position <= segment.start_position() {
            return Some((
                segment.start_beat(),
                segment.start_evidence,
                segment.start_uncertainty,
            ));
        }
        segment.beat_at(position)
    }

    /// Every whole beat this set states, in coordinate order, with its position.
    ///
    /// A grid is consumed as the sequence of beats it asserts — drawn on a
    /// waveform, counted into ticks — and each beat travels with the ordinal
    /// the set gives it, so one beat has one number wherever it is read. A
    /// beat landing on a seam belongs to the segment that follows it, so it is
    /// answered once; the final beat of the set is answered by its last
    /// segment.
    pub fn beats(&self) -> impl Iterator<Item = (Beat, MapPosition)> + '_ {
        let last = self.segments.last();
        let spans = self.segments.iter().flat_map(|segment| {
            let first = f64::from(segment.start_beat()).ceil();
            let count = (f64::from(segment.end_beat()) - first)
                .ceil()
                .max(0.0)
                .to_usize()
                .unwrap_or(0);
            (0..count)
                .filter_map(move |step| Beat::new(step.to_f64()? + first).ok())
                .filter_map(|beat| Some((beat, segment.position_at(beat)?.0)))
        });
        let tail = last
            .filter(|segment| f64::from(segment.end_beat()).fract() == 0.0)
            .and_then(|segment| {
                let beat = segment.end_beat();
                Some((beat, segment.position_at(beat)?.0))
            });
        spans.chain(tail)
    }

    pub(crate) fn uncovered_region(&self, position: MapPosition) -> MapRegion {
        let upper = self
            .segments
            .partition_point(|segment| segment.start_position() <= position);
        let previous = upper
            .checked_sub(1)
            .and_then(|index| self.segments.get(index));
        let next = self.segments.get(upper);
        match (previous, next) {
            (Some(previous), Some(next)) => {
                MapRegion::between(previous.end_position(), next.start_position())
            }
            _ => MapRegion::point(position),
        }
    }

    pub(crate) fn uncovered_region_by_beat(&self, beat: Beat) -> MapRegion {
        let upper = self
            .segments
            .partition_point(|segment| segment.start_beat() <= beat);
        let previous = upper
            .checked_sub(1)
            .and_then(|index| self.segments.get(index));
        let next = self.segments.get(upper);
        match (previous, next) {
            (Some(previous), Some(next)) => {
                MapRegion::between(previous.end_position(), next.start_position())
            }
            (Some(previous), None) => MapRegion::point(previous.end_position()),
            (None, Some(next)) => MapRegion::point(next.start_position()),
            (None, None) => MapRegion::point(self.zero_position()),
        }
    }

    fn zero_position(&self) -> MapPosition {
        match self.axis {
            MapAxis::Asset(_) => MapPosition::Asset(AssetFrame::ZERO),
            MapAxis::Session(_) => MapPosition::Session(SessionFrame::new(0)),
        }
    }
}

/// Covers every unmarked span of `axis` with extrapolated segments.
///
/// A grid the analysis marked only in part still has to answer, so the spans
/// no marker describes are cut into beats of their neighbour's own spacing:
/// the head before the first marked beat, every interior gap, and the tail to
/// the end of a bounded axis. The session axis is unbounded, so it has no head
/// and no tail to reach — only its interior gaps are covered.
///
/// # Errors
///
/// Returns [`SegmentError::InvalidTempo`] when a neighbour carries no spacing
/// to extend, or when the beat the span reaches is unrepresentable. An unmarked
/// span is not a hole to route around: a grid that cannot describe one is a
/// broken grid.
fn extended(axis: MapAxis, segments: Vec<MapSegment>) -> Result<Vec<MapSegment>, SegmentError> {
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return Ok(segments);
    };
    let mut extended: Vec<MapSegment> = Vec::with_capacity(segments.len() * 2 + 1);
    extended.extend(head_segment(first)?);
    for (index, pair) in segments.windows(2).enumerate() {
        extended.push(pair[0].clone());
        extended.extend(gap_segment(index + 1, &pair[0], &pair[1])?);
    }
    let tail = tail_segment(segments.len() - 1, axis, last)?;
    extended.push(last.clone());
    extended.extend(tail);
    Ok(extended)
}

/// The span from the start of the axis to the first marked beat.
fn head_segment(first: &MapSegment) -> Result<Option<MapSegment>, SegmentError> {
    let start = match first.start_position() {
        MapPosition::Asset(frame) if f64::from(frame) > 0.0 => MapPosition::Asset(AssetFrame::ZERO),
        MapPosition::Asset(_) | MapPosition::Session(_) => return Ok(None),
    };
    let spacing = spacing_of(0, first)?;
    let frames = f64::try_from(first.start_position()).map_err(|_| unrepresentable(0))?;
    let start_beat = Beat::new(f64::from(first.start_beat()) - frames / spacing)
        .map_err(|_| unrepresentable(0))?;
    Ok(MapSegment::extrapolated(
        (start, start_beat),
        (first.start_position(), first.start_beat()),
        first.start_uncertainty,
        first.meter_facts(),
    ))
}

/// The span between two marked segments that do not touch.
fn gap_segment(
    index: usize,
    previous: &MapSegment,
    next: &MapSegment,
) -> Result<Option<MapSegment>, SegmentError> {
    spacing_of(index, previous)?;
    Ok(MapSegment::extrapolated(
        (previous.end_position(), previous.end_beat()),
        (next.start_position(), next.start_beat()),
        previous.end_uncertainty,
        previous.meter_facts(),
    ))
}

/// The span from the last marked beat to the end of a bounded axis.
fn tail_segment(
    index: usize,
    axis: MapAxis,
    last: &MapSegment,
) -> Result<Option<MapSegment>, SegmentError> {
    let MapAxis::Asset(asset) = axis else {
        return Ok(None);
    };
    let frame_count = asset
        .frame_count()
        .to_f64()
        .ok_or_else(|| unrepresentable(index))?;
    let end = AssetFrame::new(frame_count).map_err(|_| unrepresentable(index))?;
    let spacing = spacing_of(index, last)?;
    let frames =
        f64::from(end) - f64::try_from(last.end_position()).map_err(|_| unrepresentable(index))?;
    let end_beat = Beat::new(f64::from(last.end_beat()) + frames / spacing)
        .map_err(|_| unrepresentable(index))?;
    Ok(MapSegment::extrapolated(
        (last.end_position(), last.end_beat()),
        (MapPosition::Asset(end), end_beat),
        last.end_uncertainty,
        last.meter_facts(),
    ))
}

/// The frames one beat of `neighbour` occupies, which an unmarked span reuses.
fn spacing_of(index: usize, neighbour: &MapSegment) -> Result<f64, SegmentError> {
    neighbour
        .frames_per_beat()
        .ok_or_else(|| unrepresentable(index))
}

/// Names the segment whose spacing an unmarked span could not be cut from.
const fn unrepresentable(index: usize) -> SegmentError {
    SegmentError::InvalidTempo { index }
}
