use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::SourceSpan;
use kithara_stretch::{ElasticEngine, ElasticError, ElasticRequest};

use super::{
    super::trajectory::Fraction,
    core::WarpRenderer,
    mapping::{fraction, span_speed},
};

#[cfg(all(test, feature = "stretch-signalsmith"))]
#[path = "tests/projected_tests.rs"]
mod tests;

pub(in crate::render) struct Projection {
    pub(in crate::render) engines: [Box<dyn ElasticEngine>; 2],
    pub(in crate::render) buffers: [SampleBuffer; 2],
    pub(in crate::render) stages: usize,
    pub(in crate::render) feed: Option<u64>,
    pub(in crate::render) offset: usize,
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

fn sample_projected_frame(
    resident: &super::residency::SourceResidency,
    position: Option<Fraction>,
    next: Option<Fraction>,
    span: SourceSpan,
    terminal_source_end: Option<u64>,
    output: &mut [f32],
) -> Result<(), ElasticError> {
    let Some(position) = position else {
        output.fill(0.0);
        return Ok(());
    };
    let source = u64::try_from(position.numerator / position.denominator.get())
        .map_err(|_| ElasticError::SampleCountOverflow)?;
    if terminal_source_end.is_some_and(|end| source >= end) {
        output.fill(0.0);
        return Ok(());
    }
    let speed = if let Some(next) = next {
        next.sub(position)
            .and_then(Fraction::as_f64)
            .ok_or(ElasticError::SampleCountOverflow)?
    } else {
        span_speed(span, 0)?
    };
    super::super::source_sample::source_frame(
        resident,
        (position.numerator, position.denominator),
        speed,
        terminal_source_end,
        output,
    )
}

fn process_projection_followers(
    engines: &mut [Box<dyn ElasticEngine>; 2],
    factors: [f64; 3],
    stages: usize,
    first: &[f32],
    second: &mut [f32],
    output: &mut [f32],
) -> Result<(), ElasticError> {
    if stages == 2 {
        engines[0].process(ElasticRequest::new(1, 1)?, first, output)?;
    } else {
        engines[0].process(ElasticRequest::new(1, 1)?, first, second)?;
        engines[1].set_pitch(factors[2])?;
        engines[1].process(ElasticRequest::new(1, 1)?, second, output)?;
    }
    Ok(())
}

fn process_projected_frame(
    engine: &mut dyn ElasticEngine,
    projection: Option<&mut Projection>,
    factors: [f64; 3],
    stages: usize,
    input: &[f32],
    output: &mut [f32],
    channels: usize,
) -> Result<(), ElasticError> {
    if stages == 1 {
        engine.process(ElasticRequest::new(1, 1)?, input, output)?;
    } else {
        let projection = projection.ok_or(ElasticError::PoolCapacity)?;
        let [first, second] = &mut projection.buffers;
        first.truncate(channels);
        second.truncate(channels);
        engine.process(ElasticRequest::new(1, 1)?, input, first)?;
        projection.engines[0].set_pitch(factors[1])?;
        process_projection_followers(
            &mut projection.engines,
            factors,
            stages,
            first,
            second,
            output,
        )?;
    }
    Ok(())
}

fn prime_projection_followers(
    projection: &mut Projection,
    discarded: &mut SampleBuffer,
    channels: usize,
    history_and_lead: usize,
    factors: [f64; 3],
) -> Result<(), ElasticError> {
    let [first, second] = &mut projection.buffers;
    if projection.stages > 1 {
        projection.engines[0].set_pitch(factors[1])?;
        prime_stage(
            projection.engines[0].as_mut(),
            first,
            second,
            discarded,
            channels,
            history_and_lead * (projection.stages - 2),
        )?;
    }
    if projection.stages > 2 {
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
    Ok(())
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    fn project_source_segment(
        &mut self,
        span: SourceSpan,
        offset: i64,
        output: std::ops::Range<usize>,
        nominal: Option<SourceSpan>,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        if output.is_empty() {
            return Ok(());
        }
        let source_start = span.start();
        let mut position = nominal
            .map(|nominal| {
                self.map_nominal_position(
                    source_start,
                    nominal
                        .source_ratio_at(0)
                        .ok_or(ElasticError::SampleCountOverflow)?,
                )
            })
            .transpose()?;
        for (frame, output) in output.enumerate() {
            let frame_offset =
                i64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?;
            let offset = offset
                .checked_add(frame_offset)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let frame = u64::try_from(frame).map_err(|_| ElasticError::SampleCountOverflow)?;
            let next = nominal
                .map(|nominal| {
                    self.map_nominal_position(
                        source_start,
                        nominal
                            .source_ratio_at(frame + 1)
                            .ok_or(ElasticError::SampleCountOverflow)?,
                    )
                })
                .transpose()?;
            let current = match nominal {
                Some(_) => position,
                None => resident.history_position(offset.unsigned_abs()),
            };
            let pending = self
                .pending_source
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?;
            let output = output * channels;
            sample_projected_frame(
                resident,
                current.map(fraction),
                next.map(fraction),
                span,
                self.terminal_source_end,
                &mut pending[output..output + channels],
            )?;
            position = next;
        }
        Ok(())
    }

    fn prepare_projection_buffer(&mut self, frames: usize) -> Result<(), ElasticError> {
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
        Ok(())
    }

    fn project_source(
        &mut self,
        span: SourceSpan,
        from: i64,
        frames: usize,
    ) -> Result<(), ElasticError> {
        self.prepare_projection_buffer(frames)?;
        let mut cursor = self.trajectory.clone();
        cursor.advance_by(
            span.start(),
            self.spec.sample_rate,
            from.max(0).unsigned_abs(),
        )?;
        self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let mut first = 0;
        while first < frames {
            let offset = from
                .checked_add(i64::try_from(first).map_err(|_| ElasticError::SampleCountOverflow)?)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let count = if offset < 0 {
                usize::try_from(offset.unsigned_abs())
                    .map_err(|_| ElasticError::SampleCountOverflow)?
                    .min(frames - first)
            } else {
                cursor.output_limit(frames - first)
            };
            let nominal = if offset < 0 {
                None
            } else {
                Some(cursor.span(span.start(), self.spec.sample_rate, count)?)
            };
            self.project_source_segment(span, offset, first..first + count, nominal)?;
            if let Some(nominal) = nominal {
                cursor.advance(nominal)?;
            }
            first += count;
        }
        Ok(())
    }

    fn render_projected_frame(
        &mut self,
        speed: f64,
        frame: usize,
        channels: usize,
        stages: usize,
    ) -> Result<(), ElasticError> {
        let factors = pitch_factors(speed, stages);
        self.apply_pitch(factors[0])?;
        let input = self
            .pending_source
            .as_deref()
            .ok_or(ElasticError::PoolCapacity)?;
        let input = &input[frame * channels..(frame + 1) * channels];
        let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        let output = &mut scratch[frame * channels..(frame + 1) * channels];
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        process_projected_frame(
            engine.as_mut(),
            self.projection.as_mut(),
            factors,
            stages,
            input,
            output,
            channels,
        )?;
        Ok(())
    }

    fn prepare_projected_output(
        &mut self,
        span: SourceSpan,
        lookahead: usize,
        channels: usize,
    ) -> Result<usize, ElasticError> {
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
        self.project_source(
            span,
            i64::try_from(lookahead).map_err(|_| ElasticError::SampleCountOverflow)?,
            frames,
        )?;
        Ok(frames)
    }

    pub(in crate::render) fn render_projected(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        if let Some(speed) = self.native_projection_speed(capabilities, self.current_keylock)? {
            return self.render_native_projection(span, speed);
        }
        let channels = usize::from(self.spec.channels);
        let latency = capabilities.latency();
        let stages = self.projection_stages()?;
        let lookahead = latency
            .first()
            .checked_add(latency.second())
            .and_then(|frames| frames.checked_mul(stages))
            .ok_or(ElasticError::SampleCountOverflow)?;
        if !self.active {
            self.prime_projected(span, stages)?;
        }
        let frames = self.prepare_projected_output(span, lookahead, channels)?;
        let affine_speed = (frames != 0 && span.is_affine())
            .then(|| span_speed(span, 0))
            .transpose()?;
        let mut position = span.source_ratio_at(0).map(fraction);
        for frame in 0..frames {
            let speed = if let Some(speed) = affine_speed {
                speed
            } else {
                let next = u64::try_from(frame)
                    .ok()
                    .and_then(|frame| frame.checked_add(1))
                    .and_then(|frame| span.source_ratio_at(frame))
                    .map(fraction);
                let speed = position
                    .zip(next)
                    .and_then(|(position, next)| next.sub(position))
                    .and_then(Fraction::as_f64)
                    .ok_or(ElasticError::SampleCountOverflow)?;
                position = next;
                speed
            };
            self.render_projected_frame(speed, frame, channels, stages)?;
        }
        self.clear_pending_source();
        self.active = true;
        Ok(())
    }

    fn prime_projected_stages(
        &mut self,
        stages: usize,
        history: usize,
        lead: usize,
        channels: usize,
        factors: [f64; 3],
    ) -> Result<(), ElasticError> {
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
        projection.feed = None;
        let first = &mut projection.buffers[0];
        let next = history
            .checked_add(lead)
            .and_then(|frames| frames.checked_mul(stages - 1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        prime_stage(engine.as_mut(), input, first, discarded, channels, next)?;
        prime_projection_followers(projection, discarded, channels, history + lead, factors)?;
        Ok(())
    }

    fn prepare_prime_source(
        &mut self,
        span: SourceSpan,
        history: usize,
        warm: usize,
        stages: usize,
    ) -> Result<usize, ElasticError> {
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
        Ok(lead)
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
        let lead = self.prepare_prime_source(span, history, warm, stages)?;
        let factors = pitch_factors(
            self.mapped_frame_speed(span.start(), self.spec.sample_rate, 0)?,
            stages,
        );
        self.apply_pitch(factors[0])?;
        self.prime_projected_stages(stages, history, lead, channels, factors)?;
        self.clear_pending_source();
        self.active = true;
        Ok(())
    }
}
