use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunkInfo, FrameCount};
use kithara_stretch::ElasticError;
use kithara_test_macros as kithara;

use super::{
    core::{PreparedQuantum, WarpRenderer},
    mapping::source_demand,
};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Select source demand up to the next output quantum's mapped endpoint.
    /// Filter and engine lookahead remain in the separately prepared source window.
    ///
    /// # Errors
    /// Returns pending activation or the geometry/engine admission error.
    pub fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<FrameCount, crate::WarpRenderError> {
        self.prepare_source_window(meta, remaining, output_limit)?;
        let prepared = self
            .prepared_quantum
            .ok_or(crate::WarpRenderError::OutstandingQuantum)?;
        Ok(FrameCount::new(prepared.active_frames))
    }

    /// Prepare the decoded-source window, including filter and engine lookahead.
    /// Supply this physical extent to `render_quantum`; only its mapped span is emitted.
    ///
    /// # Errors
    /// Returns the quantum's preparation or mapping error.
    pub fn prepare_source_window(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<FrameCount, crate::WarpRenderError> {
        if self.terminal_error.is_some() {
            return Err(crate::WarpRenderError::Terminated);
        }
        if meta.source_span.is_some_and(|span| !span.is_affine()) {
            return Err(crate::WarpRenderError::UnsupportedSourceGeometry);
        }
        if let Some(frames) = self.prepared_source_window(meta, output_limit)? {
            return Ok(frames);
        }
        self.rebase_source(meta);
        if self.transition_pending() || self.engine_outdated() {
            return Err(crate::WarpRenderError::NeedsService);
        }
        if !self.requires_staging() && self.plan.is_some() {
            return Err(crate::WarpRenderError::UnsupportedRegionPlan);
        }
        self.terminal_source_end = None;
        let prepared = self.prepare_active_quantum(meta, remaining, output_limit)?;
        self.prepared_quantum = Some(prepared);
        Ok(FrameCount::new(prepared.frames))
    }

    fn prepared_source_window(
        &mut self,
        meta: AudioChunkInfo,
        output_limit: usize,
    ) -> Result<Option<FrameCount>, crate::WarpRenderError> {
        if let Some(prepared) = self.prepared_quantum {
            if prepared.source_start != meta.frame_offset {
                return Err(crate::WarpRenderError::OutstandingQuantum);
            }
            if prepared.source_span.is_none_or(|span| {
                usize::try_from(span.output_frames()).is_ok_and(|frames| frames <= output_limit)
            }) {
                return Ok(Some(FrameCount::new(prepared.frames)));
            }
            self.prepared_quantum = None;
        }
        Ok(None)
    }

    fn prepare_active_quantum(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        if !self.requires_staging()
            || (self.plan.is_none()
                && (self.trajectory.constant_unity()
                    || (!self.current_keylock && self.trajectory.unity_interval())))
        {
            return self.prepare_unity_quantum(meta, remaining, output_limit);
        }
        self.prepare_projected_quantum(meta, remaining, output_limit)
    }

    fn prepare_unity_quantum(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        // One lane packet, whatever the decoded chunk holds: staging fills it
        // across chunks. Only an unquantised render follows the chunk.
        let staging = self.requires_staging();
        let packet = match self.render_quantum_frames {
            Some(quantum) => quantum.get().min(self.source_block_frames.get()),
            None if staging => remaining.min(self.source_block_frames.get()),
            None => remaining,
        };
        let output_limit = if staging {
            self.trajectory.output_limit(output_limit)
        } else {
            output_limit
        };
        let outputs = packet.min(output_limit);
        if outputs == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let span = self.mapping_span(meta.frame_offset, meta.spec.sample_rate, outputs)?;
        let (numerator, denominator) = span
            .source_ratio_at(span.output_frames() - 1)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let end = u64::try_from(numerator.div_ceil(denominator.get()))
            .ok()
            .and_then(|frame| frame.checked_add(1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frames = usize::try_from(end.saturating_sub(meta.frame_offset))
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        let active_frames = source_demand(span, meta.frame_offset)?;
        let frames = frames.max(active_frames);
        Ok(PreparedQuantum {
            source_span: Some(span),
            rate: self.rate,
            speed: 1.0,
            source_start: meta.frame_offset,
            active_frames,
            frames,
        })
    }

    fn prepare_projected_quantum(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let output_limit = self.trajectory.output_limit(output_limit);
        let quantum = self.render_quantum_frames.map_or_else(
            || remaining.min(capabilities.max_output_frames()),
            |limit| limit.get().min(capabilities.max_output_frames()),
        );
        let outputs = output_limit
            .min(quantum)
            .min(self.source_block_frames.get());
        if outputs == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let span = self.mapping_span(meta.frame_offset, meta.spec.sample_rate, outputs)?;
        let outputs =
            usize::try_from(span.output_frames()).map_err(|_| ElasticError::SampleCountOverflow)?;
        let lookahead = if self.current_keylock {
            self.projection_lookahead()?
        } else {
            0
        };
        let offset = lookahead
            .checked_add(outputs - 1)
            .and_then(|offset| u64::try_from(offset).ok())
            .ok_or(ElasticError::SampleCountOverflow)?;
        let (numerator, denominator) =
            self.mapped_position(meta.frame_offset, meta.spec.sample_rate, offset)?;
        let radius = self.projection_radius()?;
        let end = u64::try_from(numerator.div_ceil(denominator.get()))
            .ok()
            .and_then(|frame| frame.checked_add(radius + 1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frames = usize::try_from(end.saturating_sub(meta.frame_offset))
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        let active_frames = source_demand(span, meta.frame_offset)?;
        let frames = frames.max(active_frames);
        Ok(PreparedQuantum {
            source_span: Some(span),
            rate: self.rate,
            speed: self.trajectory.speed()?,
            source_start: meta.frame_offset,
            active_frames,
            frames,
        })
    }

    fn projection_radius(&self) -> Result<u64, ElasticError> {
        let correction = self.plan.as_ref().map_or(1.0, |plan| {
            plan.segments().iter().fold(1.0_f64, |minimum, segment| {
                minimum.min(segment.ratio_correction())
            })
        });
        let radius = if f64::from(self.trajectory.speed_bounds()?.1) <= correction {
            0
        } else {
            u64::try_from(crate::consts::SOURCE_RADIUS)
                .map_err(|_| ElasticError::SampleCountOverflow)?
        };
        Ok(radius)
    }

    #[kithara::hang_watchdog]
    fn terminal_span(
        span: kithara_signal::SourceSpan,
        end: u64,
    ) -> Option<kithara_signal::SourceSpan> {
        let mut outputs = span.output_frames();
        while outputs > 0 {
            hang_tick!();
            let (numerator, denominator) = span.source_ratio_at(outputs - 1)?;
            if numerator / denominator.get() < u128::from(end) {
                break;
            }
            outputs -= 1;
            hang_reset!();
        }
        span.for_output_range(0..outputs)
    }

    /// Shrink a prepared source span at true EOF without sampling controls again.
    #[kithara::hang_watchdog]
    pub fn prepare_terminal_quantum(&mut self, frames: usize) -> Option<FrameCount> {
        let mut prepared = self.prepared_quantum.take()?;
        if frames == 0 || frames > prepared.frames {
            return None;
        }
        let end = prepared
            .source_start
            .checked_add(u64::try_from(frames).ok()?)?;
        self.terminal_source_end = Some(end);
        if let Some(span) = prepared.source_span {
            prepared.source_span = Some(Self::terminal_span(span, end)?);
        }
        let shrink = prepared.frames - frames;
        prepared.frames = frames;
        prepared.active_frames = match prepared.source_span {
            Some(span) => source_demand(span, prepared.source_start).ok()?.min(frames),
            None if prepared.active_frames > shrink => prepared.active_frames - shrink,
            None => frames,
        };
        self.prepared_quantum = Some(prepared);
        Some(FrameCount::new(frames))
    }
}
