use std::ops::Range;

use kithara_stretch::{ElasticSpanPlan, ElasticSpanRequest};

use super::{
    Active, ElasticCopyError, ElasticPlanError, ElasticRenderError, ElasticRenderOutcome,
    ElasticRenderSegment, ElasticRenderer, PlaybackDirection, RenderContext, SourceRange,
    TrackBinding, TransportBoundary, TransportRevision, plan_elastic_segments, sample_count,
};
use crate::resource::Resource;

pub(super) struct SourceCopy<'a> {
    pub(super) fetch: &'a [f32],
    pub(super) target: &'a mut [f32],
    pub(super) direction: PlaybackDirection,
    pub(super) fetch_range: SourceRange,
    pub(super) start: i64,
    pub(super) source_frame_count: u64,
    pub(super) channels: usize,
    pub(super) frames: usize,
}

impl ElasticRenderer<Active> {
    fn integer_segments(
        &self,
        planned: &[ElasticRenderSegment],
        revision: TransportRevision,
    ) -> Result<ElasticSpanPlan, ElasticRenderError> {
        if planned
            .iter()
            .any(|segment| segment.request.revision() != revision)
        {
            return Err(ElasticRenderError::RevisionMismatch);
        }
        Ok(ElasticSpanPlan::new(
            planned.iter().map(|segment| segment.request.span()),
            Some(self.state.runtime.prepared.cursor),
            self.backend.capabilities(),
        )?)
    }

    pub(crate) fn render(
        &mut self,
        source: &mut Resource,
        binding: &TrackBinding,
        context: &RenderContext,
        output_range: Range<usize>,
        output: &mut [&mut [f32]],
    ) -> Result<ElasticRenderOutcome, ElasticRenderError> {
        if self.state.runtime.prepared.direction != binding.direction() {
            return Err(ElasticRenderError::DirectionMismatch);
        }
        if output.len() != self.backend.capabilities().channels() {
            return Err(ElasticRenderError::OutputChannelMismatch);
        }
        let commit = context
            .transport_commit()
            .ok_or(ElasticRenderError::TransportCommitUnavailable)?;
        let revision = commit.revision();
        self.poll_relocation_read(source)?;
        if let Some((prepared_revision, target)) = self
            .state
            .runtime
            .relocation
            .as_ref()
            .map(|relocation| (relocation.revision, relocation.target))
        {
            if prepared_revision == revision
                && commit.boundary() == TransportBoundary::Relocate(target)
            {
                self.commit_relocation(revision)?;
            } else if revision >= prepared_revision {
                self.discard_relocation(prepared_revision);
            }
        }
        if revision < self.state.runtime.prepared.revision {
            return Err(ElasticRenderError::RevisionMismatch);
        }
        let envelope = self.backend.capabilities().rate_envelope();
        let requested_frames = output_range.len();
        let requested_end = output_range.end;
        let planned = match plan_elastic_segments(
            binding,
            context,
            output_range,
            self.state.runtime.prepared.request_id,
            revision,
            envelope,
        ) {
            Ok(planned) => planned,
            Err(ElasticPlanError::OutsideMarkerDomain { .. }) => {
                return Ok(ElasticRenderOutcome::Eof);
            }
            Err(error) => return Err(error.into()),
        };
        let rendered_end = planned
            .last()
            .and_then(|segment| {
                segment
                    .output_start
                    .checked_add(segment.request.span().output_frames())
            })
            .ok_or(ElasticRenderError::FrameOverflow)?;
        if rendered_end < requested_end {
            for channel in output.iter_mut() {
                channel[rendered_end..requested_end].fill(0.0);
            }
        }
        let direction = binding.direction();
        let plan = self.integer_segments(&planned, revision)?;
        let segments = plan.segments();
        let source_start = segments
            .iter()
            .flat_map(|segment| [segment.source_start(), segment.source_end()])
            .min()
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let source_end = segments
            .iter()
            .flat_map(|segment| [segment.source_start(), segment.source_end()])
            .max()
            .ok_or(ElasticRenderError::FrameOverflow)?;
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
        let fetch_range = SourceRange::try_from(source_start..source_end)?;
        self.ensure_window(source, fetch_range, direction)?;
        let source_window = self.state.runtime.prepared.source_window;
        let fetch_frames =
            usize::try_from(source_window.len()).map_err(|_| ElasticRenderError::FrameOverflow)?;
        if fetch_frames > self.max_fetch_frames {
            return Err(ElasticRenderError::FetchWindowMismatch);
        }
        let fetch_samples = sample_count(fetch_frames, self.backend.capabilities().channels())
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        for (planned, segment) in planned.iter().zip(segments) {
            self.render_segment(
                *segment,
                planned.output_start,
                direction,
                source_window,
                fetch_samples,
                output,
            )?;
        }

        self.state.runtime.prepared.cursor = plan.cursor();
        self.state.runtime.prepared.revision = revision;
        self.state.runtime.prepared.request_id = planned
            .last()
            .ok_or(ElasticRenderError::FrameOverflow)?
            .request
            .request_id()
            .checked_add(1)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        Ok(ElasticRenderOutcome::Ready {
            frames: requested_frames,
        })
    }

    fn render_segment(
        &mut self,
        segment: ElasticSpanRequest,
        output_start: usize,
        direction: PlaybackDirection,
        fetch_range: SourceRange,
        fetch_samples: usize,
        output: &mut [&mut [f32]],
    ) -> Result<(), ElasticRenderError> {
        let channels = self.backend.capabilities().channels();
        let request = segment.request();
        let source_frames = request.source_frames();
        if source_frames > self.max_source_frames {
            return Err(ElasticRenderError::FrameOverflow);
        }
        SourceCopy {
            direction,
            fetch_range,
            channels,
            start: segment.source_start(),
            frames: source_frames,
            fetch: &self.fetch[..fetch_samples],
            target: &mut self.source,
            source_frame_count: self.source_frame_count,
        }
        .copy()
        .map_err(ElasticRenderError::from)?;
        let source_samples =
            sample_count(source_frames, channels).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let output_samples = sample_count(request.output_frames(), channels)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        self.backend.process(
            request,
            &self.source[..source_samples],
            &mut self.output[..output_samples],
        )?;
        let output_end = output_start
            .checked_add(request.output_frames())
            .ok_or(ElasticRenderError::FrameOverflow)?;
        for (channel, target) in output.iter_mut().enumerate() {
            let source_channel = channel.min(channels - 1);
            for (frame, sample) in target[output_start..output_end].iter_mut().enumerate() {
                *sample = self.output[frame * channels + source_channel];
            }
        }
        Ok(())
    }
}

impl SourceCopy<'_> {
    pub(super) fn copy(self) -> Result<(), ElasticCopyError> {
        let Self {
            start,
            frames,
            direction,
            fetch_range,
            fetch,
            target,
            channels,
            source_frame_count,
        } = self;
        let samples = frames
            .checked_mul(channels)
            .ok_or(ElasticCopyError::FrameOverflow)?;
        if target.len() < samples {
            return Err(ElasticCopyError::FrameOverflow);
        }
        for frame in 0..frames {
            let offset = i64::try_from(frame).map_err(|_| ElasticCopyError::FrameOverflow)?;
            let coordinate = match direction {
                PlaybackDirection::Forward => start.checked_add(offset),
                PlaybackDirection::Reverse => start
                    .checked_sub(offset)
                    .and_then(|value| value.checked_sub(1)),
            }
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
            let coordinate =
                u64::try_from(coordinate).map_err(|_| ElasticCopyError::FrameOverflow)?;
            if coordinate >= source_frame_count {
                target[target_start..target_end].fill(0.0);
                continue;
            }
            if coordinate < fetch_range.start().get() || coordinate >= fetch_range.end().get() {
                return Err(ElasticCopyError::FetchWindowMismatch);
            }
            let source_frame = usize::try_from(coordinate - fetch_range.start().get())
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
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn forward_copy_indexes_from_the_actual_window_start() {
        let range = SourceRange::try_from(2..6).expect("valid source range");
        let source = [20.0, 30.0, 40.0, 50.0];
        let mut output = [0.0; 2];

        SourceCopy {
            start: 4,
            frames: 2,
            direction: PlaybackDirection::Forward,
            fetch_range: range,
            fetch: &source,
            target: &mut output,
            channels: 1,
            source_frame_count: 6,
        }
        .copy()
        .expect("forward copy succeeds");

        assert_eq!(output, [40.0, 50.0]);
    }

    #[kithara::test]
    fn reverse_copy_consumes_an_ascending_window_in_descending_order() {
        let range = SourceRange::try_from(2..6).expect("valid source range");
        let source = [20.0, 30.0, 40.0, 50.0];
        let mut output = [0.0; 3];

        SourceCopy {
            start: 6,
            frames: 3,
            direction: PlaybackDirection::Reverse,
            fetch_range: range,
            fetch: &source,
            target: &mut output,
            channels: 1,
            source_frame_count: 6,
        }
        .copy()
        .expect("reverse copy succeeds");

        assert_eq!(output, [50.0, 40.0, 30.0]);
    }

    #[kithara::test]
    fn reverse_copy_reaches_source_start_without_wrapping() {
        let range = SourceRange::try_from(0..2).expect("valid source range");
        let source = [10.0, 20.0];
        let mut output = [f32::NAN; 4];

        SourceCopy {
            start: 2,
            frames: 4,
            direction: PlaybackDirection::Reverse,
            fetch_range: range,
            fetch: &source,
            target: &mut output,
            channels: 1,
            source_frame_count: 2,
        }
        .copy()
        .expect("reverse copy reaches source start");

        assert_eq!(output, [20.0, 10.0, 0.0, 0.0]);
    }
}
