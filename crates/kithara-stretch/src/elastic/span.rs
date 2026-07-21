use std::ops::Range;

use num_traits::ToPrimitive;
use smallvec::SmallVec;

use super::{ElasticCapabilities, ElasticError, ElasticRequest, ElasticSpanConfig};

const MAX_SPANS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceDirection {
    Forward,
    Reverse,
}

impl From<ElasticSpan> for SourceDirection {
    fn from(span: ElasticSpan) -> Self {
        if span.source_end > span.source_start {
            Self::Forward
        } else {
            Self::Reverse
        }
    }
}

impl SourceDirection {
    const fn sign(self) -> f64 {
        match self {
            Self::Forward => 1.0,
            Self::Reverse => -1.0,
        }
    }
}

/// One continuous source path mapped onto an exact output frame count.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticSpan {
    source_end: f64,
    source_start: f64,
    output_frames: usize,
}

impl ElasticSpan {
    fn direction(self) -> SourceDirection {
        self.into()
    }

    /// Exact number of output frames represented by this span.
    #[must_use]
    pub const fn output_frames(self) -> usize {
        self.output_frames
    }

    /// Continuous source coordinate at the end of this span.
    #[must_use]
    pub const fn source_end(self) -> f64 {
        self.source_end
    }

    /// Continuous source coordinate at the start of this span.
    #[must_use]
    pub const fn source_start(self) -> f64 {
        self.source_start
    }
}

impl TryFrom<(Range<f64>, usize)> for ElasticSpan {
    type Error = ElasticError;

    fn try_from((source, output_frames): (Range<f64>, usize)) -> Result<Self, Self::Error> {
        if output_frames == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let source_start = source.start;
        let source_end = source.end;
        if !source_start.is_finite() {
            return Err(ElasticError::InvalidSourceCoordinate(source_start));
        }
        if !source_end.is_finite() {
            return Err(ElasticError::InvalidSourceCoordinate(source_end));
        }
        if source_start == source_end {
            return Err(ElasticError::StationarySourceSpan);
        }
        Ok(Self {
            source_end,
            source_start,
            output_frames,
        })
    }
}

/// Persistent continuous and integer source position between exact-span blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticCursor {
    continuous: f64,
    integer: i64,
}

impl ElasticCursor {
    /// Continuous source position after the last planned block.
    #[must_use]
    pub const fn continuous(self) -> f64 {
        self.continuous
    }

    /// Integer source boundary from which the next block must continue.
    #[must_use]
    pub const fn integer(self) -> i64 {
        self.integer
    }

    fn with_integer(continuous: f64, integer: i64) -> Result<Self, ElasticError> {
        if !continuous.is_finite() {
            return Err(ElasticError::InvalidSourceCoordinate(continuous));
        }
        Ok(Self {
            continuous,
            integer,
        })
    }
}

impl TryFrom<f64> for ElasticCursor {
    type Error = ElasticError;

    fn try_from(continuous: f64) -> Result<Self, Self::Error> {
        Self::with_integer(continuous, quantize_source(continuous)?)
    }
}

/// One quantized source span ready for exact backend processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ElasticSpanRequest {
    request: ElasticRequest,
    source_end: i64,
    source_start: i64,
}

impl ElasticSpanRequest {
    /// Exact positive source/output frame counts for the backend.
    #[must_use]
    pub const fn request(self) -> ElasticRequest {
        self.request
    }

    /// Integer exclusive source boundary after this request.
    #[must_use]
    pub const fn source_end(self) -> i64 {
        self.source_end
    }

    /// Integer source boundary from which this request begins.
    #[must_use]
    pub const fn source_start(self) -> i64 {
        self.source_start
    }
}

/// Bounded integer exact-span plan and the cursor it commits on success.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ElasticSpanPlan {
    cursor: ElasticCursor,
    segments: SmallVec<[ElasticSpanRequest; MAX_SPANS]>,
}

impl ElasticSpanPlan {
    /// Maximum number of source spans accepted for one real-time render block.
    pub const MAX_SPANS: usize = MAX_SPANS;

    /// Quantizes up to four continuous source spans inside one capability envelope.
    /// # Errors
    /// Returns [`ElasticError`] for invalid continuity, direction, phase,
    /// capacity, rate-envelope, or integer arithmetic.
    pub fn new<I>(
        spans: I,
        cursor: Option<ElasticCursor>,
        capabilities: ElasticCapabilities,
        config: ElasticSpanConfig,
    ) -> Result<Self, ElasticError>
    where
        I: IntoIterator<Item = ElasticSpan>,
    {
        let mut continuous = SmallVec::<[ElasticSpan; MAX_SPANS]>::new();
        for span in spans {
            if continuous.len() == MAX_SPANS {
                return Err(ElasticError::SpanLimit { limit: MAX_SPANS });
            }
            continuous.push(span);
        }
        let first = continuous
            .first()
            .copied()
            .ok_or(ElasticError::EmptySpanPlan)?;
        let (direction, output_frames) = validate_spans(&continuous, capabilities, config)?;
        let phase = PhasePlan::new(
            &continuous,
            cursor,
            capabilities,
            config,
            direction,
            output_frames,
        )?;
        let mut boundary = cursor.map_or_else(
            || quantize_source(first.source_start),
            |cursor| Ok(cursor.integer),
        )?;
        let envelope = capabilities.rate_envelope();
        let minimum_rate = envelope.min_source_frames_per_output();
        let maximum_rate = envelope.max_source_frames_per_output();
        let mut cumulative_output_frames = 0usize;
        let mut segments = SmallVec::<[ElasticSpanRequest; MAX_SPANS]>::new();
        for span in &continuous {
            cumulative_output_frames = cumulative_output_frames
                .checked_add(span.output_frames)
                .ok_or(ElasticError::SpanArithmeticOverflow)?;
            let corrected_end = phase.corrected_end(span.source_end, cumulative_output_frames)?;
            let end = quantize_endpoint(
                boundary,
                corrected_end,
                span.output_frames,
                minimum_rate,
                maximum_rate,
                direction,
            )?;
            let source_frames = usize::try_from(boundary.abs_diff(end))
                .map_err(|_| ElasticError::SpanArithmeticOverflow)?;
            if source_frames > capabilities.max_source_frames() {
                return Err(ElasticError::SourceFrameLimit {
                    frames: source_frames,
                    limit: capabilities.max_source_frames(),
                });
            }
            segments.push(ElasticSpanRequest {
                request: ElasticRequest::new(source_frames, span.output_frames)?,
                source_start: boundary,
                source_end: end,
            });
            boundary = end;
        }
        let last = continuous
            .last()
            .copied()
            .ok_or(ElasticError::EmptySpanPlan)?;
        let cursor = ElasticCursor::with_integer(
            phase.corrected_end(last.source_end, phase.output_frames)?,
            boundary,
        )?;
        Ok(Self { cursor, segments })
    }

    /// Cursor to publish only after every planned segment renders successfully.
    #[must_use]
    pub const fn cursor(&self) -> ElasticCursor {
        self.cursor
    }

    /// Quantized source spans in the same order as the continuous input.
    #[must_use]
    pub fn segments(&self) -> &[ElasticSpanRequest] {
        &self.segments
    }
}

#[derive(Clone, Copy)]
struct PhasePlan {
    correction: f64,
    cursor_origin: f64,
    source_origin: f64,
    output_frames: usize,
}

impl PhasePlan {
    fn new(
        spans: &[ElasticSpan],
        cursor: Option<ElasticCursor>,
        capabilities: ElasticCapabilities,
        config: ElasticSpanConfig,
        direction: SourceDirection,
        output_frames: usize,
    ) -> Result<Self, ElasticError> {
        let first = spans.first().ok_or(ElasticError::EmptySpanPlan)?;
        let cursor_origin = cursor.map_or(first.source_start, ElasticCursor::continuous);
        let error = first.source_start - cursor_origin;
        if error.abs() > config.max_phase_error() {
            return Err(ElasticError::PhaseDiscontinuity {
                error,
                limit: config.max_phase_error(),
            });
        }
        let correction_budget = output_frames
            .to_f64()
            .zip(capabilities.max_output_frames().to_f64())
            .map(|(output, maximum)| (output / maximum).min(config.max_correction_per_block()))
            .filter(|budget| budget.is_finite() && *budget > 0.0)
            .ok_or(ElasticError::SpanArithmeticOverflow)?;
        let desired = error.clamp(-correction_budget, correction_budget);
        let envelope = capabilities.rate_envelope();
        let correction = constrain_correction(
            spans,
            desired,
            output_frames,
            envelope.min_source_frames_per_output(),
            envelope.max_source_frames_per_output(),
            direction,
        )?;
        if error.abs() > config.continuity_tolerance()
            && correction.abs() <= config.continuity_tolerance()
        {
            return Err(ElasticError::PhaseCorrectionUnavailable { error });
        }
        Ok(Self {
            cursor_origin,
            correction,
            output_frames,
            source_origin: first.source_start,
        })
    }

    fn corrected_end(
        self,
        source_end: f64,
        cumulative_output_frames: usize,
    ) -> Result<f64, ElasticError> {
        let progress = cumulative_output_frames
            .to_f64()
            .zip(self.output_frames.to_f64())
            .map(|(cumulative, total)| cumulative / total)
            .ok_or(ElasticError::SpanArithmeticOverflow)?;
        let corrected =
            self.cursor_origin + (source_end - self.source_origin) + self.correction * progress;
        if corrected.is_finite() {
            Ok(corrected)
        } else {
            Err(ElasticError::SpanArithmeticOverflow)
        }
    }
}

fn validate_spans(
    spans: &[ElasticSpan],
    capabilities: ElasticCapabilities,
    config: ElasticSpanConfig,
) -> Result<(SourceDirection, usize), ElasticError> {
    if let Some((previous, next)) =
        spans
            .iter()
            .zip(spans.iter().skip(1))
            .find(|(previous, next)| {
                (previous.source_end - next.source_start).abs() > config.continuity_tolerance()
            })
    {
        return Err(ElasticError::DiscontinuousSource {
            expected: previous.source_end,
            actual: next.source_start,
        });
    }
    let first = spans.first().ok_or(ElasticError::EmptySpanPlan)?;
    let direction = first.direction();
    if spans
        .iter()
        .copied()
        .any(|span| span.direction() != direction)
    {
        return Err(ElasticError::SourceDirectionChange);
    }
    let output_frames = spans.iter().try_fold(0usize, |total, span| {
        total
            .checked_add(span.output_frames)
            .ok_or(ElasticError::SpanArithmeticOverflow)
    })?;
    if output_frames > capabilities.max_output_frames() {
        return Err(ElasticError::OutputFrameLimit {
            frames: output_frames,
            limit: capabilities.max_output_frames(),
        });
    }
    let envelope = capabilities.rate_envelope();
    for span in spans {
        let output = span
            .output_frames
            .to_f64()
            .ok_or(ElasticError::SpanArithmeticOverflow)?;
        let rate = (span.source_end - span.source_start).abs() / output;
        if !envelope.contains_rate(rate) {
            return Err(ElasticError::InvalidRate(rate));
        }
    }
    Ok((direction, output_frames))
}

fn constrain_correction(
    spans: &[ElasticSpan],
    desired: f64,
    total_frames: usize,
    minimum_rate: f64,
    maximum_rate: f64,
    direction: SourceDirection,
) -> Result<f64, ElasticError> {
    let total_frames = total_frames
        .to_f64()
        .ok_or(ElasticError::SpanArithmeticOverflow)?;
    let mut positive_headroom = f64::INFINITY;
    let mut negative_headroom = f64::INFINITY;
    let sign = direction.sign();
    for span in spans {
        let frames = span
            .output_frames
            .to_f64()
            .ok_or(ElasticError::SpanArithmeticOverflow)?;
        let nominal = (span.source_end - span.source_start) * sign;
        if !nominal.is_finite() || nominal <= 0.0 {
            return Err(ElasticError::SpanArithmeticOverflow);
        }
        let scale = total_frames / frames;
        positive_headroom = positive_headroom.min((maximum_rate * frames - nominal) * scale);
        negative_headroom = negative_headroom.min((nominal - minimum_rate * frames) * scale);
    }
    let desired = desired * sign;
    let correction = if desired > 0.0 {
        desired.min(positive_headroom.max(0.0))
    } else {
        desired.max(-negative_headroom.max(0.0))
    };
    Ok(correction * sign)
}

fn quantize_endpoint(
    boundary: i64,
    continuous_end: f64,
    output_frames: usize,
    minimum_rate: f64,
    maximum_rate: f64,
    direction: SourceDirection,
) -> Result<i64, ElasticError> {
    let output_frames = output_frames
        .to_f64()
        .ok_or(ElasticError::SpanArithmeticOverflow)?;
    let minimum_span = (minimum_rate * output_frames)
        .ceil()
        .to_i64()
        .ok_or(ElasticError::SpanArithmeticOverflow)?;
    let maximum_span = (maximum_rate * output_frames)
        .floor()
        .to_i64()
        .ok_or(ElasticError::SpanArithmeticOverflow)?;
    if minimum_span <= 0 || minimum_span > maximum_span {
        return Err(ElasticError::SpanArithmeticOverflow);
    }
    let (minimum_end, maximum_end) = match direction {
        SourceDirection::Forward => (
            boundary
                .checked_add(minimum_span)
                .ok_or(ElasticError::SpanArithmeticOverflow)?,
            boundary
                .checked_add(maximum_span)
                .ok_or(ElasticError::SpanArithmeticOverflow)?,
        ),
        SourceDirection::Reverse => (
            boundary
                .checked_sub(maximum_span)
                .ok_or(ElasticError::SpanArithmeticOverflow)?,
            boundary
                .checked_sub(minimum_span)
                .ok_or(ElasticError::SpanArithmeticOverflow)?,
        ),
    };
    Ok(quantize_source(continuous_end)?.clamp(minimum_end, maximum_end))
}

fn quantize_source(source: f64) -> Result<i64, ElasticError> {
    source
        .round()
        .to_i64()
        .ok_or(ElasticError::InvalidSourceCoordinate(source))
}
