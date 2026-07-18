use std::ops::Range;

use num_traits::ToPrimitive;
use smallvec::SmallVec;

use super::{
    ElasticCopyError, ElasticRenderError, ElasticRenderOutcome, ElasticRenderSegment,
    ElasticRenderer, ElasticRequest, IntegerSegment, PlaybackDirection, RenderContext,
    SourceCursor, SourceFrameRange, TrackBinding, plan_elastic_segments, quantize_source,
    sample_count,
};

pub(super) struct SourceCopy<'a> {
    pub(super) start: i64,
    pub(super) frames: usize,
    pub(super) fetch_range: SourceFrameRange,
    pub(super) fetch: &'a [f32],
    pub(super) target: &'a mut [f32],
    pub(super) channels: usize,
    pub(super) source_frame_count: u64,
}

#[derive(Clone, Copy)]
struct ContinuousSegment {
    output_start: usize,
    output_frames: usize,
    source_start: f64,
    source_end: f64,
}

#[derive(Clone, Copy)]
struct PhasePlan {
    source_origin: f64,
    cursor_origin: f64,
    correction: f64,
    output_frames: usize,
}

impl PhasePlan {
    const CONTINUITY_EPSILON: f64 = 1.0e-6;
    const MAX_CONTINUOUS_PHASE_ERROR: f64 = 1.0;
    const MAX_CORRECTION_PER_BLOCK: f64 = 1.0;

    fn corrected_end(
        self,
        source_end: f64,
        cumulative_output_frames: usize,
    ) -> Result<f64, ElasticRenderError> {
        let progress = cumulative_output_frames
            .to_f64()
            .zip(self.output_frames.to_f64())
            .map(|(cumulative, total)| cumulative / total)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        Ok(self.cursor_origin + (source_end - self.source_origin) + self.correction * progress)
    }
}

impl ElasticRenderer {
    pub(crate) fn render(
        &mut self,
        binding: &TrackBinding,
        context: &RenderContext,
        output_range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> Result<ElasticRenderOutcome, ElasticRenderError> {
        if !self.primed {
            return Err(ElasticRenderError::NotPrepared);
        }
        if binding.direction() != PlaybackDirection::Forward {
            return Err(ElasticRenderError::ReversePreparationRequired);
        }
        if output.len() != 2 {
            return Err(ElasticRenderError::OutputChannelMismatch);
        }
        let revision = context
            .transport_commit()
            .ok_or(ElasticRenderError::TransportCommitUnavailable)?
            .revision();
        if revision < self.revision {
            return Err(ElasticRenderError::RevisionMismatch);
        }
        let envelope = self.capabilities.rate_envelope();
        let planned = plan_elastic_segments(
            binding,
            context,
            output_range,
            self.request_id,
            revision,
            envelope,
        )?;
        let (segments, staged_cursor) = self.integer_segments(&planned, revision)?;
        let first = segments
            .first()
            .copied()
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let last = segments
            .last()
            .copied()
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let source_start = first.source_start;
        let source_end = last.source_end;
        let source_extent = i64::try_from(self.source_frame_count)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        if source_end <= 0 || source_start >= source_extent {
            return Ok(ElasticRenderOutcome::Eof);
        }
        let source_start =
            u64::try_from(source_start.max(0)).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let source_end = u64::try_from(source_end.min(source_extent))
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        if source_start >= source_end {
            return Ok(ElasticRenderOutcome::Eof);
        }
        let fetch_range = SourceFrameRange::new(source_start, source_end)?;
        self.ensure_window(fetch_range)?;
        let source_window = self
            .source_window
            .ok_or(ElasticRenderError::FetchWindowMismatch)?;
        let fetch_frames =
            usize::try_from(source_window.len()).map_err(|_| ElasticRenderError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticRenderError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.channels)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        for segment in &segments {
            self.render_segment(*segment, source_window, fetch_samples, output)?;
        }

        self.cursor = Some(staged_cursor);
        self.revision = revision;
        self.request_id = planned
            .last()
            .ok_or(ElasticRenderError::FrameOverflow)?
            .request
            .request_id()
            .checked_add(1)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        Ok(ElasticRenderOutcome::Ready {
            frames: segments.iter().map(|segment| segment.output_frames).sum(),
        })
    }

    fn integer_segments(
        &self,
        planned: &[ElasticRenderSegment],
        revision: u64,
    ) -> Result<(SmallVec<[IntegerSegment; 4]>, SourceCursor), ElasticRenderError> {
        let mut continuous = SmallVec::<[ContinuousSegment; 4]>::new();
        for segment in planned {
            if segment.request.revision() != revision {
                return Err(ElasticRenderError::RevisionMismatch);
            }
            continuous.push(ContinuousSegment {
                output_start: segment.output_start,
                output_frames: segment.request.output_frames(),
                source_start: segment.request.source_start(),
                source_end: segment.request.source_end(),
            });
        }
        let envelope = self.capabilities.rate_envelope();
        quantize_segments(
            &continuous,
            self.cursor,
            self.capabilities.max_output_frames(),
            envelope.min_source_frames_per_output(),
            envelope.max_source_frames_per_output(),
        )
    }

    fn render_segment(
        &mut self,
        segment: IntegerSegment,
        fetch_range: SourceFrameRange,
        fetch_samples: usize,
        output: &mut [&mut [f32]],
    ) -> Result<(), ElasticRenderError> {
        let source_frames = usize::try_from(segment.source_start.abs_diff(segment.source_end))
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        if source_frames > self.max_source_frames {
            return Err(ElasticRenderError::FrameOverflow);
        }
        copy_source(SourceCopy {
            start: segment.source_start,
            frames: source_frames,
            fetch_range,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.source,
            channels: self.channels,
            source_frame_count: self.source_frame_count,
        })
        .map_err(ElasticRenderError::from)?;
        let request = ElasticRequest::new(source_frames, segment.output_frames)?;
        let source_samples = sample_count(source_frames, self.channels)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        let output_samples = sample_count(segment.output_frames, self.channels)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        self.backend.process(
            request,
            &self.source[..source_samples],
            &mut self.output[..output_samples],
        )?;
        let output_end = segment
            .output_start
            .checked_add(segment.output_frames)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        for (channel, target) in output.iter_mut().enumerate() {
            let source_channel = channel.min(self.channels - 1);
            for (frame, sample) in target[segment.output_start..output_end]
                .iter_mut()
                .enumerate()
            {
                *sample = self.output[frame * self.channels + source_channel];
            }
        }
        Ok(())
    }
}

fn quantize_segments(
    segments: &[ContinuousSegment],
    cursor: Option<SourceCursor>,
    max_output_frames: usize,
    minimum_rate: f64,
    maximum_rate: f64,
) -> Result<(SmallVec<[IntegerSegment; 4]>, SourceCursor), ElasticRenderError> {
    let first = segments.first().ok_or(ElasticRenderError::FrameOverflow)?;
    validate_continuity(segments)?;
    let phase = plan_phase(
        segments,
        cursor,
        max_output_frames,
        minimum_rate,
        maximum_rate,
    )?;
    let mut boundary = cursor.map_or_else(
        || quantize_source(first.source_start),
        |cursor| Ok(cursor.integer),
    )?;
    let mut cumulative_output_frames = 0usize;
    let mut result = SmallVec::<[IntegerSegment; 4]>::new();
    for segment in segments {
        cumulative_output_frames = cumulative_output_frames
            .checked_add(segment.output_frames)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let corrected_end = phase.corrected_end(segment.source_end, cumulative_output_frames)?;
        let end = quantize_endpoint(
            boundary,
            corrected_end,
            segment.output_frames,
            minimum_rate,
            maximum_rate,
        )?;
        result.push(IntegerSegment {
            output_start: segment.output_start,
            output_frames: segment.output_frames,
            source_start: boundary,
            source_end: end,
        });
        boundary = end;
    }
    let last = segments.last().ok_or(ElasticRenderError::FrameOverflow)?;
    Ok((
        result,
        SourceCursor {
            continuous: phase.corrected_end(last.source_end, phase.output_frames)?,
            integer: boundary,
        },
    ))
}

fn validate_continuity(segments: &[ContinuousSegment]) -> Result<(), ElasticRenderError> {
    for pair in segments.windows(2) {
        if (pair[0].source_end - pair[1].source_start).abs() > PhasePlan::CONTINUITY_EPSILON {
            return Err(ElasticRenderError::DiscontinuousSource {
                expected: pair[0].source_end,
                actual: pair[1].source_start,
            });
        }
    }
    Ok(())
}

fn plan_phase(
    segments: &[ContinuousSegment],
    cursor: Option<SourceCursor>,
    max_output_frames: usize,
    minimum_rate: f64,
    maximum_rate: f64,
) -> Result<PhasePlan, ElasticRenderError> {
    let first = segments.first().ok_or(ElasticRenderError::FrameOverflow)?;
    let output_frames = segments.iter().try_fold(0usize, |total, segment| {
        total
            .checked_add(segment.output_frames)
            .ok_or(ElasticRenderError::FrameOverflow)
    })?;
    let cursor_origin = cursor.map_or(first.source_start, |cursor| cursor.continuous);
    let error = first.source_start - cursor_origin;
    if error.abs() > PhasePlan::MAX_CONTINUOUS_PHASE_ERROR {
        return Err(ElasticRenderError::RelocationRequired {
            error,
            limit: PhasePlan::MAX_CONTINUOUS_PHASE_ERROR,
        });
    }
    let correction_budget = output_frames
        .to_f64()
        .zip(max_output_frames.to_f64())
        .map(|(output, maximum)| (output / maximum).min(PhasePlan::MAX_CORRECTION_PER_BLOCK))
        .filter(|budget| budget.is_finite() && *budget > 0.0)
        .ok_or(ElasticRenderError::FrameOverflow)?;
    let desired = error.clamp(-correction_budget, correction_budget);
    let correction = constrain_correction(segments, desired, minimum_rate, maximum_rate)?;
    if error.abs() > PhasePlan::CONTINUITY_EPSILON
        && correction.abs() <= PhasePlan::CONTINUITY_EPSILON
    {
        return Err(ElasticRenderError::PhaseCorrectionUnavailable { error });
    }
    Ok(PhasePlan {
        source_origin: first.source_start,
        cursor_origin,
        correction,
        output_frames,
    })
}

fn constrain_correction(
    segments: &[ContinuousSegment],
    desired: f64,
    minimum_rate: f64,
    maximum_rate: f64,
) -> Result<f64, ElasticRenderError> {
    let total_frames = segments
        .iter()
        .map(|segment| segment.output_frames)
        .sum::<usize>()
        .to_f64()
        .ok_or(ElasticRenderError::FrameOverflow)?;
    let mut positive_headroom = f64::INFINITY;
    let mut negative_headroom = f64::INFINITY;
    for segment in segments {
        let frames = segment
            .output_frames
            .to_f64()
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let nominal = segment.source_end - segment.source_start;
        if !nominal.is_finite() || nominal <= 0.0 {
            return Err(ElasticRenderError::FrameOverflow);
        }
        let scale = total_frames / frames;
        positive_headroom = positive_headroom.min((maximum_rate * frames - nominal) * scale);
        negative_headroom = negative_headroom.min((nominal - minimum_rate * frames) * scale);
    }
    if desired > 0.0 {
        Ok(desired.min(positive_headroom.max(0.0)))
    } else {
        Ok(desired.max(-negative_headroom.max(0.0)))
    }
}

fn quantize_endpoint(
    boundary: i64,
    continuous_end: f64,
    output_frames: usize,
    minimum_rate: f64,
    maximum_rate: f64,
) -> Result<i64, ElasticRenderError> {
    let output_frames = output_frames
        .to_f64()
        .ok_or(ElasticRenderError::FrameOverflow)?;
    let minimum_span = (minimum_rate * output_frames)
        .ceil()
        .to_i64()
        .ok_or(ElasticRenderError::FrameOverflow)?;
    let maximum_span = (maximum_rate * output_frames)
        .floor()
        .to_i64()
        .ok_or(ElasticRenderError::FrameOverflow)?;
    if minimum_span <= 0 || minimum_span > maximum_span {
        return Err(ElasticRenderError::FrameOverflow);
    }
    let minimum_end = boundary
        .checked_add(minimum_span)
        .ok_or(ElasticRenderError::FrameOverflow)?;
    let maximum_end = boundary
        .checked_add(maximum_span)
        .ok_or(ElasticRenderError::FrameOverflow)?;
    Ok(quantize_source(continuous_end)?.clamp(minimum_end, maximum_end))
}

pub(super) fn copy_source(copy: SourceCopy<'_>) -> Result<(), ElasticCopyError> {
    let SourceCopy {
        start,
        frames,
        fetch_range,
        fetch,
        target,
        channels,
        source_frame_count,
    } = copy;
    copy_forward(
        start,
        frames,
        fetch_range,
        fetch,
        target,
        channels,
        source_frame_count,
    )
}

fn copy_forward(
    start: i64,
    frames: usize,
    fetch_range: SourceFrameRange,
    fetch: &[f32],
    target: &mut [f32],
    channels: usize,
    source_frame_count: u64,
) -> Result<(), ElasticCopyError> {
    let samples = frames
        .checked_mul(channels)
        .ok_or(ElasticCopyError::FrameOverflow)?;
    if target.len() < samples {
        return Err(ElasticCopyError::FrameOverflow);
    }
    for frame in 0..frames {
        let coordinate = start
            .checked_add(i64::try_from(frame).map_err(|_| ElasticCopyError::FrameOverflow)?)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        let target_start = frame
            .checked_mul(channels)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        let target_end = target_start
            .checked_add(channels)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        if coordinate < 0 {
            target[target_start..target_end].fill(0.0);
            continue;
        }
        let coordinate = u64::try_from(coordinate).map_err(|_| ElasticCopyError::FrameOverflow)?;
        if coordinate >= source_frame_count {
            target[target_start..target_end].fill(0.0);
            continue;
        }
        if coordinate < fetch_range.start() || coordinate >= fetch_range.end() {
            return Err(ElasticCopyError::FetchWindowMismatch);
        }
        let source_frame = usize::try_from(coordinate - fetch_range.start())
            .map_err(|_| ElasticCopyError::FrameOverflow)?;
        let source_start = source_frame
            .checked_mul(channels)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        let source_end = source_start
            .checked_add(channels)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        target[target_start..target_end].copy_from_slice(&fetch[source_start..source_end]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Consts;

    impl Consts {
        const MAX_OUTPUT_FRAMES: usize = 480;
        const MINIMUM_RATE: f64 = 2.0 / 3.0;
        const MAXIMUM_RATE: f64 = 4.0 / 3.0;
    }

    fn segment(source_start: f64, source_end: f64, output_frames: usize) -> ContinuousSegment {
        ContinuousSegment {
            output_start: 0,
            output_frames,
            source_start,
            source_end,
        }
    }

    fn quantize(
        segments: &[ContinuousSegment],
        cursor: Option<SourceCursor>,
    ) -> Result<(SmallVec<[IntegerSegment; 4]>, SourceCursor), ElasticRenderError> {
        quantize_segments(
            segments,
            cursor,
            Consts::MAX_OUTPUT_FRAMES,
            Consts::MINIMUM_RATE,
            Consts::MAXIMUM_RATE,
        )
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= PhasePlan::CONTINUITY_EPSILON,
            "expected {expected}, received {actual}"
        );
    }

    #[test]
    fn forward_copy_indexes_from_the_actual_window_start() {
        let range = SourceFrameRange::new(2, 6).expect("valid source range");
        let source = [20.0, 30.0, 40.0, 50.0];
        let mut output = [0.0; 2];

        copy_forward(4, 2, range, &source, &mut output, 1, 6).expect("forward copy succeeds");

        assert_eq!(output, [40.0, 50.0]);
    }

    #[test]
    fn small_phase_error_converges_without_a_source_jump_and_is_partition_independent() {
        let mut cursor = SourceCursor {
            continuous: 0.0,
            integer: 0,
        };
        let mut residuals = vec![0.75];
        for start in [0.75, 120.75, 240.75] {
            let desired_end = start + 120.0;
            let (integer, next) = quantize(&[segment(start, desired_end, 120)], Some(cursor))
                .expect("small phase error is correctable");
            assert_eq!(integer[0].source_start, cursor.integer);
            residuals.push(desired_end - next.continuous);
            cursor = next;
        }

        assert_eq!(residuals, vec![0.75, 0.5, 0.25, 0.0]);

        let (integer, whole) = quantize(
            &[segment(0.75, 360.75, 360)],
            Some(SourceCursor {
                continuous: 0.0,
                integer: 0,
            }),
        )
        .expect("combined subrange is correctable");
        assert_eq!(integer[0].source_start, 0);
        assert_close(whole.continuous, cursor.continuous);
        assert_eq!(whole.integer, cursor.integer);
    }

    #[test]
    fn negative_phase_error_converges_without_overshoot() {
        let mut cursor = SourceCursor {
            continuous: 0.75,
            integer: 1,
        };
        let mut residuals = vec![-0.75];
        for start in [0.0, 120.0, 240.0] {
            let desired_end = start + 120.0;
            let (_, next) = quantize(&[segment(start, desired_end, 120)], Some(cursor))
                .expect("negative phase error is correctable");
            residuals.push(desired_end - next.continuous);
            cursor = next;
        }

        assert_eq!(residuals, vec![-0.75, -0.5, -0.25, 0.0]);
    }

    #[test]
    fn one_frame_error_is_continuous_but_larger_error_requires_relocation() {
        let cursor = Some(SourceCursor {
            continuous: 0.0,
            integer: 0,
        });
        quantize(&[segment(1.0, 121.0, 120)], cursor)
            .expect("one source frame is inside the continuous envelope");

        let above_one = f64::from_bits(1.0_f64.to_bits() + 1);
        let error = quantize(&[segment(above_one, 120.0 + above_one, 120)], cursor)
            .expect_err("larger error requires prepared relocation");
        assert!(matches!(
            error,
            ElasticRenderError::RelocationRequired { error, limit }
                if error > limit && limit == PhasePlan::MAX_CONTINUOUS_PHASE_ERROR
        ));
    }

    #[test]
    fn correction_respects_backend_rate_headroom() {
        let cursor = Some(SourceCursor {
            continuous: 0.0,
            integer: 0,
        });
        let error = quantize(&[segment(0.75, 160.75, 120)], cursor)
            .expect_err("maximum nominal rate has no positive headroom");
        assert!(matches!(
            error,
            ElasticRenderError::PhaseCorrectionUnavailable { error }
                if (error - 0.75).abs() <= PhasePlan::CONTINUITY_EPSILON
        ));

        let (integer, corrected) = quantize(&[segment(0.75, 160.65, 120)], cursor)
            .expect("partial headroom permits partial correction");
        assert_eq!(integer[0].source_end - integer[0].source_start, 160);
        assert_close(corrected.continuous, 160.0);
        assert_close(160.65 - corrected.continuous, 0.65);
    }

    #[test]
    fn planned_segment_gap_is_not_treated_as_phase_error() {
        let error = quantize(
            &[segment(0.0, 120.0, 120), segment(120.5, 240.5, 120)],
            None,
        )
        .expect_err("planner segments must remain strictly adjacent");
        assert!(matches!(
            error,
            ElasticRenderError::DiscontinuousSource {
                expected: 120.0,
                actual: 120.5
            }
        ));
    }
}
