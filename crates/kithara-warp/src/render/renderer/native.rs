use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::SourceSpan;
use kithara_stretch::{ElasticCapabilities, ElasticError, ElasticRequest};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::{core::WarpRenderer, residency::SourceResidency};

fn copy_source(
    resident: &SourceResidency,
    output: &mut SampleBuffer,
    start: u64,
    end: u64,
    terminal: Option<u64>,
    channels: usize,
) -> Result<(), ElasticError> {
    let count = usize::try_from(
        end.checked_sub(start)
            .ok_or(ElasticError::SampleCountOverflow)?,
    )
    .ok()
    .and_then(|frames| frames.checked_mul(channels))
    .ok_or(ElasticError::SampleCountOverflow)?;
    if count > output.capacity() {
        return Err(ElasticError::PoolCapacity);
    }
    output
        .ensure_len(count)
        .map_err(|_| ElasticError::PoolCapacity)?;
    output.truncate(count);
    output.fill(0.0);
    let physical_end = terminal.map_or(end, |terminal| terminal.min(end));
    if physical_end > start {
        let range = resident.range(
            i64::try_from(start).map_err(|_| ElasticError::SampleCountOverflow)?,
            physical_end,
            channels,
        )?;
        output[..range.len()].copy_from_slice(&resident.samples[range]);
    }
    Ok(())
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn native_projection_speed(
        &self,
        capabilities: ElasticCapabilities,
        keylock: bool,
    ) -> Result<Option<f64>, ElasticError> {
        if !keylock || self.plan.is_some() || self.trajectory.requires_quanta() {
            return Ok(None);
        }
        let speed = f64::from(self.trajectory.speed()?);
        if speed.recip() >= crate::consts::MAX_NATIVE_STRETCH {
            return Ok(None);
        }
        Ok(capabilities
            .rate_envelope()
            .contains_rate(speed)
            .then_some(speed))
    }

    fn native_boundary(&self, span: SourceSpan, output: usize) -> Result<u64, ElasticError> {
        let latency = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        let offset = latency
            .second()
            .checked_add(output)
            .and_then(|frames| u64::try_from(frames).ok())
            .ok_or(ElasticError::SampleCountOverflow)?;
        let (numerator, denominator) =
            self.mapped_position(span.start(), self.spec.sample_rate, offset)?;
        let maximum = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .rate_envelope()
            .max_source_frames_per_output();
        let source = if f64::from(self.trajectory.speed()?) == maximum {
            numerator / denominator.get()
        } else {
            numerator.div_ceil(denominator.get())
        };
        u64::try_from(source)
            .ok()
            .and_then(|source| source.checked_add(u64::try_from(latency.first()).ok()?))
            .ok_or(ElasticError::SampleCountOverflow)
    }

    pub(in crate::render) fn native_projection_packet(
        &self,
        speed: f64,
        capabilities: ElasticCapabilities,
    ) -> Result<usize, ElasticError> {
        let envelope = capabilities.rate_envelope();
        let source = self
            .source_block_frames
            .get()
            .min(capabilities.max_source_frames());
        let output = crate::consts::NATIVE_PACKET_FRAMES.min(capabilities.max_output_frames());
        if let Some(boundary) = [
            envelope.min_source_frames_per_output(),
            envelope.max_source_frames_per_output(),
        ]
        .into_iter()
        .find(|boundary| boundary.to_f32() == speed.to_f32())
        {
            return envelope
                .largest_request_at(boundary, source, output)
                .map(|request| request.output_frames())
                .ok_or(ElasticError::InvalidRate(speed));
        }
        ((source.saturating_sub(1))
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?
            / speed)
            .floor()
            .to_usize()
            .map(|frames| frames.min(output))
            .filter(|frames| *frames > 0)
            .ok_or(ElasticError::EmptyOutput)
    }

    fn prepare_native_history(
        &mut self,
        cue: u64,
        history_start: i64,
        history_frames: usize,
        channels: usize,
    ) -> Result<usize, ElasticError> {
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let history = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        let history_samples = history_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        history
            .ensure_len(history_samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        history.truncate(history_samples);
        history.fill(0.0);
        let origin = i64::try_from(resident.origin.ok_or(ElasticError::EmptySource)?)
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        let available = resident.range(history_start.max(origin), cue, channels)?;
        let source_history = &resident.samples[available];
        let missing = history.len() - source_history.len();
        history[missing..].copy_from_slice(source_history);
        Ok(history_samples)
    }

    fn prepare_native_input(
        &mut self,
        cue: u64,
        feed: u64,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let source = &mut self
            .projection
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .buffers[1];
        copy_source(
            resident,
            source,
            cue,
            feed,
            self.terminal_source_end,
            channels,
        )
    }

    fn prime_native_engine(
        &mut self,
        feed: u64,
        lookahead_end: u64,
        history_samples: usize,
        warm_frames: usize,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let history = self
            .pending_source
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?;
        let source = &self
            .projection
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)?
            .buffers[1];
        let (lookahead, warm) = source.split_at(history_samples);
        let discarded = self
            .activation_scratch
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        let discarded_samples = warm_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        discarded
            .ensure_len(discarded_samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        discarded.truncate(discarded_samples);
        let request = ElasticRequest::new(
            usize::try_from(feed - lookahead_end).map_err(|_| ElasticError::SampleCountOverflow)?,
            warm_frames,
        )?;
        self.engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .prime(request, history, lookahead, warm, discarded)?;
        Ok(())
    }

    fn prime_native_projection(&mut self, span: SourceSpan) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let latency = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency();
        let (numerator, denominator) = span
            .source_ratio_at(0)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let cue = u64::try_from(numerator / denominator.get())
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        let lookahead_end = cue
            .checked_add(
                u64::try_from(latency.first()).map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        let feed = self.native_boundary(span, 0)?;
        let history_start = i64::try_from(cue)
            .ok()
            .and_then(|cue| cue.checked_sub(i64::try_from(latency.first()).ok()?))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let history_samples =
            self.prepare_native_history(cue, history_start, latency.first(), channels)?;
        self.prepare_native_input(cue, feed, channels)?;
        self.prime_native_engine(
            feed,
            lookahead_end,
            history_samples,
            latency.second(),
            channels,
        )?;
        let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
        projection.feed = Some(feed);
        projection.offset = 0;
        projection.buffers[0].clear();
        self.clear_pending_source();
        self.active = true;
        Ok(())
    }

    fn check_native_origin(&self, span: SourceSpan) -> Result<(), ElasticError> {
        let expected = self.mapped_position(span.start(), self.spec.sample_rate, 0)?;
        let actual = span
            .source_ratio_at(0)
            .ok_or(ElasticError::SampleCountOverflow)?;
        if actual != expected {
            return Err(ElasticError::DiscontinuousSource {
                expected: expected
                    .0
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?
                    / expected
                        .1
                        .get()
                        .to_f64()
                        .ok_or(ElasticError::SampleCountOverflow)?,
                actual: actual.0.to_f64().ok_or(ElasticError::SampleCountOverflow)?
                    / actual
                        .1
                        .get()
                        .to_f64()
                        .ok_or(ElasticError::SampleCountOverflow)?,
            });
        }
        Ok(())
    }

    fn prepare_native_output(
        &mut self,
        span: SourceSpan,
        speed: f64,
    ) -> Result<(usize, usize, usize), ElasticError> {
        let channels = usize::from(self.spec.channels);
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
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let packet = self.native_projection_packet(speed, capabilities)?;
        Ok((frames, channels, packet))
    }

    fn feed_native_packet(
        &mut self,
        span: SourceSpan,
        frame: usize,
        packet: usize,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let end = self.native_boundary(span, frame + packet)?;
        let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
        let start = projection.feed.ok_or(ElasticError::EmptySource)?;
        let source_frames = usize::try_from(
            end.checked_sub(start)
                .ok_or(ElasticError::SampleCountOverflow)?,
        )
        .map_err(|_| ElasticError::SampleCountOverflow)?;
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let [output, source] = &mut projection.buffers;
        copy_source(
            resident,
            source,
            start,
            end,
            self.terminal_source_end,
            channels,
        )?;
        let count = packet
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        if count > output.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        output
            .ensure_len(count)
            .map_err(|_| ElasticError::PoolCapacity)?;
        output.truncate(count);
        self.engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .process(ElasticRequest::new(source_frames, packet)?, source, output)?;
        projection.feed = Some(end);
        projection.offset = 0;
        Ok(())
    }

    #[kithara::hang_watchdog]
    pub(in crate::render) fn render_native_projection(
        &mut self,
        span: SourceSpan,
        speed: f64,
    ) -> Result<(), ElasticError> {
        self.check_native_origin(span)?;
        self.apply_pitch(1.0)?;
        if !self.active {
            self.prime_native_projection(span)?;
        }
        let (frames, channels, packet) = self.prepare_native_output(span, speed)?;
        for frame in 0..frames {
            hang_tick!();
            let projection = self.projection.as_ref().ok_or(ElasticError::PoolCapacity)?;
            if projection.offset == projection.buffers[0].len() {
                self.feed_native_packet(span, frame, packet, channels)?;
            }
            let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
            let output = &projection.buffers[0][projection.offset..projection.offset + channels];
            self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?
                [frame * channels..(frame + 1) * channels]
                .copy_from_slice(output);
            projection.offset += channels;
            hang_reset!();
        }
        Ok(())
    }
}
