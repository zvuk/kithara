use firewheel_core::param::smoother::SmoothedParam;
use kithara_bufpool::HasPool;
use kithara_signal::AudioChunkInfo;
use kithara_stretch::{
    ElasticCursor, ElasticError, ElasticRequest, ElasticSpan, ElasticSpanConfig,
};
use num_traits::ToPrimitive;

use super::renderer::{PreparedActivation, PreparedExact, PreparedQuantum, WarpRenderer};
use crate::temporal::RateTarget;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn preview_speed_from(
        mut applied_speed: SmoothedParam,
        target: f32,
        output_frames: usize,
    ) -> Result<(f64, f32, SmoothedParam), ElasticError> {
        if output_frames == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        applied_speed.set_value(target);
        let mut total = 0.0_f64;
        for _ in 0..output_frames {
            total += f64::from(applied_speed.next_smoothed());
        }
        applied_speed.settle();
        let output_frames = output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let speed = (total / output_frames)
            .to_f32()
            .filter(|speed| speed.is_finite() && *speed > 0.0)
            .ok_or(ElasticError::InvalidRate(total / output_frames))?;
        Ok((total, speed, applied_speed))
    }

    pub(super) fn advance_speed(
        applied_speed: SmoothedParam,
        target: f32,
        output_frames: usize,
    ) -> Result<SmoothedParam, ElasticError> {
        if output_frames == 0 {
            let mut held = applied_speed;
            held.set_value(target);
            return Ok(held);
        }
        Self::preview_speed_from(applied_speed, target, output_frames)
            .map(|(_, _, preview)| preview)
    }

    pub(super) fn output_quantum_limit(&self) -> usize {
        self.engine.as_ref().map_or_else(
            || self.render_quantum_frames.get(),
            |engine| {
                engine
                    .capabilities()
                    .max_output_frames()
                    .min(self.render_quantum_frames.get())
            },
        )
    }

    fn settled_output_quantum_limit(&self, applied_speed: SmoothedParam, target: f32) -> usize {
        let output_limit = self.output_quantum_limit();
        if !applied_speed.has_settled_at(target) {
            return output_limit;
        }
        let Some(capabilities) = self.engine.as_ref().map(|engine| engine.capabilities()) else {
            return output_limit;
        };
        let envelope = capabilities.rate_envelope();
        let minimum = envelope.min_source_frames_per_output();
        let maximum = envelope.max_source_frames_per_output();
        let rate = if minimum.to_f32() == Some(target) {
            minimum
        } else if maximum.to_f32() == Some(target) {
            maximum
        } else {
            return output_limit;
        };
        envelope
            .largest_request_at(rate, capabilities.max_source_frames(), output_limit)
            .map_or(output_limit, |request| request.output_frames())
    }

    fn build_exact_plan(
        &self,
        meta: AudioChunkInfo,
        output_frames: usize,
        applied_speed: SmoothedParam,
        cursor: Option<ElasticCursor>,
        rate: RateTarget,
    ) -> Result<PreparedExact, ElasticError> {
        let target = rate.speed();
        let (source_advance, speed, next_speed) =
            Self::preview_speed_from(applied_speed, target, output_frames)?;
        let source_start = cursor.map_or_else(
            || {
                meta.frame_offset
                    .to_f64()
                    .ok_or(ElasticError::SpanArithmeticOverflow)
            },
            |cursor| Ok(cursor.continuous()),
        )?;
        let source_end = source_start + source_advance;
        let span = ElasticSpan::try_from((source_start..source_end, output_frames))?;
        let capabilities = self
            .engine
            .as_ref()
            .map(|engine| engine.capabilities())
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        let config = ElasticSpanConfig::builder().build()?;
        let plan = kithara_stretch::ElasticSpanPlan::new([span], cursor, capabilities, config)?;
        Ok(PreparedExact {
            activation: None,
            next_speed,
            plan,
            rate,
            snapshot: None,
            speed,
        })
    }

    pub(super) fn prepared_source_frames(
        prepared: &PreparedQuantum,
    ) -> Result<usize, ElasticError> {
        match prepared {
            PreparedQuantum::Exact(exact) => Self::exact_source_frames(exact),
            PreparedQuantum::FrameCount { source_frames, .. } => Ok(*source_frames),
        }
    }

    pub(super) fn prepared_input_frames(prepared: &PreparedQuantum) -> Result<usize, ElasticError> {
        let source = Self::prepared_source_frames(prepared)?;
        match prepared.activation() {
            Some(activation) => source
                .checked_add(activation.prefix_frames()?)
                .ok_or(ElasticError::SampleCountOverflow),
            None => Ok(source),
        }
    }

    fn exact_source_frames(exact: &PreparedExact) -> Result<usize, ElasticError> {
        exact
            .plan
            .segments()
            .iter()
            .try_fold(0usize, |total, segment| {
                total
                    .checked_add(segment.request().source_frames())
                    .ok_or(ElasticError::SpanArithmeticOverflow)
            })
    }

    pub(super) fn exact_plan_for_remaining(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        applied_speed: SmoothedParam,
        cursor: Option<ElasticCursor>,
        rate: RateTarget,
    ) -> Result<Option<PreparedExact>, ElasticError> {
        if remaining == 0 {
            return Err(ElasticError::EmptySource);
        }
        let output_limit = self.settled_output_quantum_limit(applied_speed, rate.speed());
        let full = self.build_exact_plan(meta, output_limit, applied_speed, cursor, rate)?;
        if Self::exact_source_frames(&full)? <= remaining {
            return Ok(Some(full));
        }

        let mut best = None;
        let mut low = 1usize;
        let mut high = output_limit.saturating_sub(1);
        while low <= high {
            let output_frames = low + (high - low) / 2;
            let candidate =
                self.build_exact_plan(meta, output_frames, applied_speed, cursor, rate)?;
            let source_frames =
                candidate
                    .plan
                    .segments()
                    .iter()
                    .try_fold(0usize, |total, segment| {
                        total
                            .checked_add(segment.request().source_frames())
                            .ok_or(ElasticError::SpanArithmeticOverflow)
                    })?;
            if source_frames <= remaining {
                best = Some(candidate);
                low = output_frames.saturating_add(1);
            } else if output_frames == 1 {
                break;
            } else {
                high = output_frames - 1;
            }
        }
        Ok(best)
    }

    fn exact_plan_enabled(&self, target: f32) -> bool {
        let channels = usize::from(self.spec.channels.max(1));
        self.plan.is_none()
            && self.pending_frames(channels) == 0
            && self.output_remainder == 0.0
            && !self.can_passthrough(target)
    }

    fn frame_count_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        applied_speed: SmoothedParam,
        rate: RateTarget,
    ) -> Result<PreparedQuantum, ElasticError> {
        let target = rate.speed();
        let (_, speed, _) =
            Self::preview_speed_from(applied_speed, target, self.output_quantum_limit())?;
        let source_frames = self.source_frames_for_quantum(meta, remaining, speed)?;
        Ok(PreparedQuantum::FrameCount {
            activation: None,
            source_frames,
            rate,
            snapshot: None,
            speed,
        })
    }

    pub(super) fn direct_plan(
        &mut self,
        meta: AudioChunkInfo,
        frames: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        let rate = self.controls.rate_target();
        let target = rate.speed();
        if frames > 0
            && self.exact_plan_enabled(target)
            && let Some(exact) = self.exact_plan_for_remaining(
                meta,
                frames,
                self.applied_speed,
                self.exact_cursor,
                rate,
            )?
        {
            return Ok(PreparedQuantum::Exact(exact));
        }
        let (_, speed, _) =
            Self::preview_speed_from(self.applied_speed, target, self.output_quantum_limit())?;
        Ok(PreparedQuantum::FrameCount {
            activation: None,
            source_frames: frames,
            rate,
            snapshot: None,
            speed,
        })
    }

    pub(super) fn scheduler_plan_at(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        rate: RateTarget,
    ) -> Result<PreparedQuantum, ElasticError> {
        let target = rate.speed();
        if self.exact_plan_enabled(target)
            && let Some(exact) = self.exact_plan_for_remaining(
                meta,
                remaining,
                self.applied_speed,
                self.exact_cursor,
                rate,
            )?
        {
            return Ok(PreparedQuantum::Exact(exact));
        }
        self.frame_count_quantum(meta, remaining, self.applied_speed, rate)
    }

    fn prepared_activation(
        &self,
        prepared: &PreparedQuantum,
    ) -> Result<Option<PreparedActivation>, ElasticError> {
        if self.unity_passthrough(prepared.speed()) {
            return Ok(None);
        }
        let Some((history_frames, output_frames)) = self.activation_latency_frames() else {
            return Ok(None);
        };
        let speed = f64::from(prepared.speed());
        if !speed.is_finite() || speed <= 0.0 {
            return Err(ElasticError::InvalidRate(speed));
        }
        let source_frames = output_frames
            .to_f64()
            .map(|frames| (frames * speed).round())
            .and_then(|frames| frames.to_usize())
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(Some(PreparedActivation {
            history_frames,
            warm: ElasticRequest::new(source_frames, output_frames)?,
        }))
    }

    pub(super) fn scheduler_plan(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        let rate = self.controls.rate_target();
        let preview = self.scheduler_plan_at(meta, remaining, rate)?;
        let Some(activation) = self.prepared_activation(&preview)? else {
            return Ok(preview);
        };
        let prefix = activation.prefix_frames()?;
        let shifted = Self::meta_at_frame(
            meta,
            meta.frame_offset
                .checked_add(u64::try_from(prefix).map_err(|_| ElasticError::SampleCountOverflow)?)
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        let mut prepared = self.scheduler_plan_at(shifted, remaining, rate)?;
        prepared.bind_activation(activation);
        Ok(prepared)
    }

    pub(super) fn source_block_limit(
        stretch: f64,
        max_source_frames: usize,
        max_output_frames: usize,
    ) -> Result<usize, ElasticError> {
        if !stretch.is_finite() || stretch <= 0.0 {
            return Err(ElasticError::InvalidRate(stretch));
        }
        // Frame-count partitioning may carry almost one output frame from an
        // earlier sub-frame span, so leave that frame outside this block.
        let output_limit = max_output_frames
            .checked_sub(1)
            .ok_or(ElasticError::InvalidOutputFrameLimit)?
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_limit = (output_limit / stretch)
            .floor()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_limit = source_limit.min(max_source_frames);
        if source_limit == 0 {
            return Err(ElasticError::InvalidRate(1.0 / stretch));
        }
        Ok(source_limit)
    }

    pub(super) fn balanced_source_block(remaining: usize, limit: usize) -> usize {
        let partitions = remaining.div_ceil(limit);
        remaining.div_ceil(partitions)
    }
}
