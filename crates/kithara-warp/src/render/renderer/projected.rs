use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::SourceSpan;
use kithara_stretch::{ElasticEngine, ElasticError, ElasticRequest};

use super::core::WarpRenderer;

pub(in crate::render) struct Projection {
    pub(in crate::render) engines: [Box<dyn ElasticEngine>; 2],
    pub(in crate::render) buffers: [SampleBuffer; 2],
    pub(in crate::render) stages: usize,
}

fn pitch_factors(speed: f64, stages: usize) -> [f64; 3] {
    let mut remaining = speed.recip();
    let mut factors = [1.0; 3];
    for factor in &mut factors[..stages] {
        *factor = remaining.clamp(0.25, 4.0);
        remaining /= *factor;
    }
    factors
}

fn prime_stage(
    engine: &mut dyn ElasticEngine,
    input: &[f32],
    output: &mut SampleBuffer,
    discarded: &mut SampleBuffer,
    channels: usize,
    output_frames: usize,
) -> Result<(), ElasticError> {
    let latency = engine.capabilities().latency();
    let history = latency
        .first()
        .checked_mul(channels)
        .ok_or(ElasticError::SampleCountOverflow)?;
    let warm = latency
        .second()
        .checked_mul(channels)
        .ok_or(ElasticError::SampleCountOverflow)?;
    discarded
        .ensure_len(warm)
        .map_err(|_| ElasticError::PoolCapacity)?;
    let (history_input, future) = input.split_at(history);
    let (lookahead, future) = future.split_at(history);
    let (warm_input, source) = future.split_at(warm);
    engine.prime(
        ElasticRequest::new(latency.second(), latency.second())?,
        history_input,
        lookahead,
        warm_input,
        discarded,
    )?;
    let count = output_frames
        .checked_mul(channels)
        .ok_or(ElasticError::SampleCountOverflow)?;
    if count > output.capacity() || count > source.len() {
        return Err(ElasticError::PoolCapacity);
    }
    output
        .ensure_len(count)
        .map_err(|_| ElasticError::PoolCapacity)?;
    for (input, output) in source[..count]
        .chunks_exact(channels)
        .zip(output.chunks_exact_mut(channels))
    {
        engine.process(ElasticRequest::new(1, 1)?, input, output)?;
    }
    Ok(())
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    fn project_source(
        &mut self,
        span: SourceSpan,
        from: i64,
        frames: usize,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let count = frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let pending = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        if count > pending.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        pending
            .ensure_len(count)
            .map_err(|_| ElasticError::PoolCapacity)?;
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        for frame in 0..frames {
            let offset = from
                .checked_add(i64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let position = if offset < 0 {
                resident.history_position(offset.unsigned_abs())
            } else {
                Some(self.mapped_position(
                    span.start(),
                    self.spec.sample_rate,
                    offset.unsigned_abs(),
                )?)
            };
            let Some((numerator, denominator)) = position else {
                let pending = self
                    .pending_source
                    .as_mut()
                    .ok_or(ElasticError::PoolCapacity)?;
                pending[frame * channels..(frame + 1) * channels].fill(0.0);
                continue;
            };
            let source = u64::try_from(numerator / denominator.get())
                .map_err(|_| ElasticError::SampleCountOverflow)?;
            if self.terminal_source_end.is_some_and(|end| source >= end) {
                let pending = self
                    .pending_source
                    .as_mut()
                    .ok_or(ElasticError::PoolCapacity)?;
                pending[frame * channels..(frame + 1) * channels].fill(0.0);
                continue;
            }
            let speed = self.mapped_frame_speed(
                span.start(),
                self.spec.sample_rate,
                offset.max(0).unsigned_abs(),
            )?;
            let pending = self
                .pending_source
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?;
            for channel in 0..channels {
                pending[frame * channels + channel] = super::super::source_sample::source_sample(
                    resident,
                    (numerator, denominator),
                    speed,
                    self.terminal_source_end,
                    channels,
                    channel,
                )?;
            }
        }
        Ok(())
    }

    pub(in crate::render) fn render_projected(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let latency = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        let stages = self.projection_stages()?;
        let lookahead = latency
            .first()
            .checked_add(latency.second())
            .and_then(|frames| frames.checked_mul(stages))
            .ok_or(ElasticError::SampleCountOverflow)?;
        if !self.active {
            self.prime_projected(span, stages)?;
        }
        let frames =
            usize::try_from(span.output_frames()).map_err(|_| ElasticError::SampleCountOverflow)?;
        let count = frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        if count > scratch.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        scratch
            .ensure_len(count)
            .map_err(|_| ElasticError::PoolCapacity)?;
        scratch.truncate(count);
        for frame in 0..frames {
            let offset = lookahead
                .checked_add(frame)
                .and_then(|offset| i64::try_from(offset).ok())
                .ok_or(ElasticError::SampleCountOverflow)?;
            self.project_source(span, offset, 1)?;
            let speed = self.mapped_frame_speed(
                span.start(),
                self.spec.sample_rate,
                u64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?,
            )?;
            let factors = pitch_factors(speed, stages);
            self.apply_pitch(factors[0])?;
            let input = self
                .pending_source
                .as_deref()
                .ok_or(ElasticError::PoolCapacity)?;
            let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
            let output = &mut scratch[frame * channels..(frame + 1) * channels];
            let engine = self
                .engine
                .as_mut()
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
            if stages == 1 {
                engine.process(ElasticRequest::new(1, 1)?, input, output)?;
            } else {
                let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
                let [first, second] = &mut projection.buffers;
                first.truncate(channels);
                second.truncate(channels);
                engine.process(ElasticRequest::new(1, 1)?, input, first)?;
                projection.engines[0].set_pitch(factors[1])?;
                if stages == 2 {
                    projection.engines[0].process(ElasticRequest::new(1, 1)?, first, output)?;
                } else {
                    projection.engines[0].process(ElasticRequest::new(1, 1)?, first, second)?;
                    projection.engines[1].set_pitch(factors[2])?;
                    projection.engines[1].process(ElasticRequest::new(1, 1)?, second, output)?;
                }
            }
        }
        self.clear_pending_source();
        self.active = true;
        Ok(())
    }

    fn prime_projected(&mut self, span: SourceSpan, stages: usize) -> Result<(), ElasticError> {
        let latency = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        let history = latency.first();
        let warm = latency.second();
        if history == 0 || warm == 0 {
            return Ok(());
        }
        let channels = usize::from(self.spec.channels);
        let lead = history
            .checked_add(warm)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let prefix = history
            .checked_mul(stages)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let future = lead
            .checked_mul(stages)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let from = i64::try_from(prefix)
            .map_err(|_| ElasticError::SampleCountOverflow)?
            .checked_neg()
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.project_source(
            span,
            from,
            prefix
                .checked_add(future)
                .ok_or(ElasticError::SampleCountOverflow)?,
        )?;
        let factors = pitch_factors(
            self.mapped_frame_speed(span.start(), self.spec.sample_rate, 0)?,
            stages,
        );
        self.apply_pitch(factors[0])?;
        let input = self
            .pending_source
            .as_deref()
            .ok_or(ElasticError::PoolCapacity)?;
        let discarded = self
            .activation_scratch
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
        projection.stages = stages;
        let [first, second] = &mut projection.buffers;
        let next = history
            .checked_add(lead)
            .and_then(|frames| frames.checked_mul(stages - 1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        prime_stage(engine.as_mut(), input, first, discarded, channels, next)?;
        if stages > 1 {
            projection.engines[0].set_pitch(factors[1])?;
            prime_stage(
                projection.engines[0].as_mut(),
                first,
                second,
                discarded,
                channels,
                (history + lead) * (stages - 2),
            )?;
        }
        if stages > 2 {
            projection.engines[1].set_pitch(factors[2])?;
            prime_stage(
                projection.engines[1].as_mut(),
                second,
                first,
                discarded,
                channels,
                0,
            )?;
        }
        first
            .ensure_len(channels)
            .map_err(|_| ElasticError::PoolCapacity)?;
        second
            .ensure_len(channels)
            .map_err(|_| ElasticError::PoolCapacity)?;
        self.clear_pending_source();
        self.active = true;
        Ok(())
    }
}
