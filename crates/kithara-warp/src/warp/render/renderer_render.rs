use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunkInfo, FrameCount, SampleCount};
use kithara_stretch::{ElasticCapabilities, ElasticError, ElasticRequest};
use num_traits::ToPrimitive;

use super::renderer::WarpRenderer;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    fn append_pending_source(
        &mut self,
        source: &[f32],
        meta: AudioChunkInfo,
        frame_offset: u64,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels.max(1));
        let pending_frames = self.pending_frames(channels);
        if let Some(start) = self.pending_meta {
            let expected = start
                .frame_offset
                .checked_add(
                    u64::try_from(pending_frames).map_err(|_| ElasticError::SampleCountOverflow)?,
                )
                .ok_or(ElasticError::SampleCountOverflow)?;
            if expected != frame_offset {
                return Err(ElasticError::DiscontinuousSource {
                    expected: expected.to_f64().ok_or(ElasticError::SampleCountOverflow)?,
                    actual: frame_offset
                        .to_f64()
                        .ok_or(ElasticError::SampleCountOverflow)?,
                });
            }
        }
        let pending = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        let start = pending.len();
        let end = start
            .checked_add(source.len())
            .ok_or(ElasticError::SampleCountOverflow)?;
        if end > pending.capacity() {
            return Err(ElasticError::SourceFrameLimit {
                frames: end / channels,
                limit: pending.capacity() / channels,
            });
        }
        pending
            .ensure_len(end)
            .map_err(|_| ElasticError::PoolCapacity)?;
        pending[start..end].copy_from_slice(source);
        self.pending_meta
            .get_or_insert_with(|| Self::meta_at_frame(meta, frame_offset));
        Ok(())
    }

    pub(super) fn balanced_source_block(remaining: usize, limit: usize) -> usize {
        let partitions = remaining.div_ceil(limit);
        remaining.div_ceil(partitions)
    }

    fn fit_output_to_envelope(
        source_frames: usize,
        output_frames: usize,
        remainder: f64,
        capabilities: ElasticCapabilities,
    ) -> Result<(usize, f64), ElasticError> {
        let source_frames_f = source_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let envelope = capabilities.rate_envelope();
        let minimum_output = (source_frames_f / envelope.max_source_frames_per_output())
            .ceil()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let maximum_output = (source_frames_f / envelope.min_source_frames_per_output())
            .floor()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?
            .min(capabilities.max_output_frames());
        if minimum_output == 0 || minimum_output > maximum_output {
            return Err(ElasticError::InvalidRate(source_frames_f));
        }
        let bounded = output_frames.clamp(minimum_output, maximum_output);
        let displaced = output_frames
            .to_f64()
            .and_then(|output| bounded.to_f64().map(|bounded| output - bounded))
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((bounded, remainder + displaced))
    }

    pub(super) fn output_frames(
        source_frames: usize,
        stretch: f64,
        remainder: f64,
    ) -> Result<(usize, f64), ElasticError> {
        if !stretch.is_finite() || stretch <= 0.0 {
            return Err(ElasticError::InvalidRate(stretch.recip()));
        }
        let source_frames = source_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let exact = source_frames.mul_add(stretch, remainder);
        if !exact.is_finite() {
            return Err(ElasticError::SampleCountOverflow);
        }
        // Backends require a non-empty output. Keep a sub-frame source span
        // pending until its cumulative exact output reaches one full frame;
        // EOF rounds the final residual once.
        let output_frames = if exact < 1.0 { 0.0 } else { exact.round() };
        let output_frames = output_frames
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let emitted = output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((output_frames, exact - emitted))
    }

    pub(super) fn source_block_limit(
        stretch: f64,
        capabilities: ElasticCapabilities,
        output_limit: usize,
    ) -> Result<usize, ElasticError> {
        if !stretch.is_finite() || stretch <= 0.0 {
            return Err(ElasticError::InvalidRate(stretch));
        }
        let envelope = capabilities.rate_envelope();
        let rate = stretch.recip();
        let boundary = [
            envelope.min_source_frames_per_output(),
            envelope.max_source_frames_per_output(),
        ]
        .into_iter()
        .find(|boundary| boundary.to_f32() == rate.to_f32());
        if let Some(request) = boundary.and_then(|boundary| {
            envelope.largest_request_at(boundary, capabilities.max_source_frames(), output_limit)
        }) {
            return Ok(request.source_frames());
        }
        let output_limit = output_limit
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let output_budget = (output_limit - Self::OUTPUT_ROUNDING_MARGIN).max(1.0);
        let source_limit = (output_budget / stretch)
            .floor()
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_limit = source_limit.min(capabilities.max_source_frames());
        if source_limit == 0 {
            return Err(ElasticError::InvalidRate(1.0 / stretch));
        }
        Ok(source_limit)
    }

    pub(super) fn source_frames_for_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        speed: f32,
    ) -> Result<usize, ElasticError> {
        if remaining == 0 {
            return Err(ElasticError::EmptySource);
        }
        if !self.active
            && !self.transition_pending()
            && self.pending_frames(usize::from(self.spec.channels.max(1))) == 0
            && self.unity_passthrough(speed)
        {
            return Ok(self
                .render_quantum_frames
                .map_or(remaining, |frames| remaining.min(frames.get())));
        }

        let channels = usize::from(self.spec.channels.max(1));
        let region = self.region_for(meta.frame_offset);
        let region_frames = usize::try_from(
            region
                .end()
                .checked_sub(meta.frame_offset)
                .ok_or(ElasticError::SampleCountOverflow)?
                .min(u64::try_from(remaining).map_err(|_| ElasticError::SampleCountOverflow)?),
        )
        .map_err(|_| ElasticError::SampleCountOverflow)?;
        if region_frames == 0 {
            return Err(ElasticError::StationarySourceSpan);
        }
        let stretch = 1.0 / f64::from(speed);
        let capabilities = self
            .engine
            .as_ref()
            .map(|engine| engine.capabilities())
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        let output_limit = self.render_quantum_frames.map_or_else(
            || capabilities.max_output_frames(),
            |frames| capabilities.max_output_frames().min(frames.get()),
        );
        let source_limit = Self::source_block_limit(stretch, capabilities, output_limit)?;
        let pending_frames = self.pending_frames(channels);
        let available =
            source_limit
                .checked_sub(pending_frames)
                .ok_or(ElasticError::SourceFrameLimit {
                    frames: pending_frames,
                    limit: source_limit,
                })?;
        if available == 0 {
            return Err(ElasticError::InvalidRate(stretch.recip()));
        }
        Ok(region_frames.min(available))
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn render_active(
        &mut self,
        meta: AudioChunkInfo,
        samples: &[f32],
        speed: f32,
        channels: usize,
        frames: usize,
    ) -> Result<(), ElasticError> {
        let mut consumed = 0usize;
        let mut frame = meta.frame_offset;
        for _ in 0..frames {
            if consumed == frames {
                return Ok(());
            }
            let region = self.region_for(frame);
            let left = u64::try_from(frames - consumed).unwrap_or(u64::MAX);
            let span = region.end().saturating_sub(frame).min(left).max(1);
            let rate = self
                .rate_context
                .as_ref()
                .map_or_else(|| f64::from(speed), |context| context.rate_for(region));
            let stretch = 1.0 / rate;
            self.apply_pitch(if self.controls.keylock() { 1.0 } else { rate })?;
            let capabilities = self
                .engine
                .as_ref()
                .map(|engine| engine.capabilities())
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
            let source_limit =
                Self::source_block_limit(stretch, capabilities, capabilities.max_output_frames())?;
            let remaining = usize::try_from(span).unwrap_or(frames - consumed);
            let pending_frames = self.pending_frames(channels);
            let available =
                source_limit
                    .checked_sub(pending_frames)
                    .ok_or(ElasticError::SourceFrameLimit {
                        frames: pending_frames,
                        limit: source_limit,
                    })?;
            if available == 0 {
                return Err(ElasticError::InvalidRate(stretch.recip()));
            }
            let sub = Self::balanced_source_block(remaining, available);
            let (output_frames, next_remainder) =
                Self::output_frames(sub, stretch, self.output_remainder)?;
            let part = &samples[consumed * channels..(consumed + sub) * channels];
            if output_frames == 0 {
                self.append_pending_source(part, meta, frame)?;
                self.output_remainder = next_remainder;
                consumed += sub;
                frame = frame.saturating_add(
                    u64::try_from(sub).map_err(|_| ElasticError::SampleCountOverflow)?,
                );
                continue;
            }
            if output_frames > capabilities.max_output_frames() {
                return Err(ElasticError::OutputFrameLimit {
                    frames: output_frames,
                    limit: capabilities.max_output_frames(),
                });
            }
            let source_frames = pending_frames
                .checked_add(sub)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let (output_frames, next_remainder) = Self::fit_output_to_envelope(
                source_frames,
                output_frames,
                next_remainder,
                capabilities,
            )?;
            let output_frames = FrameCount::new(output_frames);
            let request = ElasticRequest::new(source_frames, output_frames.get())?;
            let output_samples = output_frames
                .get()
                .checked_mul(channels)
                .map(SampleCount::new)
                .ok_or(ElasticError::SampleCountOverflow)?;
            let start = self.scratch.as_deref().map_or(0, <[f32]>::len);
            let end = start
                .checked_add(output_samples.get())
                .ok_or(ElasticError::SampleCountOverflow)?;
            if pending_frames > 0 {
                self.append_pending_source(part, meta, frame)?;
            }
            if start == 0 && self.output_start_meta.is_none() {
                self.output_start_meta = if pending_frames > 0 {
                    self.pending_meta
                } else {
                    Some(Self::meta_at_frame(meta, frame))
                };
            }
            let scratch = self
                .scratch
                .as_mut()
                .ok_or(ElasticError::EnginePreparation(
                    "output scratch is unavailable",
                ))?;
            if end > scratch.capacity() {
                return Err(ElasticError::OutputFrameLimit {
                    frames: end / channels,
                    limit: scratch.capacity() / channels,
                });
            }
            scratch
                .ensure_len(end)
                .map_err(|_| ElasticError::PoolCapacity)?;
            let source = self
                .pending_source
                .as_deref()
                .filter(|_| pending_frames > 0)
                .unwrap_or(part);
            let engine = self
                .engine
                .as_mut()
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
            if let Err(error) = engine.process(request, source, &mut scratch[start..end]) {
                scratch.truncate(start);
                return Err(error);
            }
            if pending_frames > 0 {
                self.clear_pending_source();
            }
            self.output_remainder = next_remainder;
            self.active = true;
            consumed += sub;
            frame = frame
                .saturating_add(u64::try_from(sub).map_err(|_| ElasticError::SampleCountOverflow)?);
        }
        if consumed == frames {
            Ok(())
        } else {
            Err(ElasticError::EnginePreparation(
                "time-stretch render exceeded its source-frame iteration bound",
            ))
        }
    }

    pub(super) fn render_terminal_pending(&mut self, channels: usize) -> Result<(), ElasticError> {
        let source_frames = self.pending_frames(channels);
        if source_frames == 0 {
            self.output_remainder = 0.0;
            return Ok(());
        }
        let output_frames = self
            .output_remainder
            .round()
            .max(0.0)
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        if output_frames == 0 {
            self.clear_pending_source();
            self.output_remainder = 0.0;
            return Ok(());
        }

        let output_frames = FrameCount::new(output_frames);
        let request = ElasticRequest::new(source_frames, output_frames.get())?;
        let output_samples = output_frames
            .get()
            .checked_mul(channels)
            .map(SampleCount::new)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let start = self.scratch.as_deref().map_or(0, <[f32]>::len);
        let end = start
            .checked_add(output_samples.get())
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self
            .scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "output scratch is unavailable",
            ))?;
        if end > scratch.capacity() {
            return Err(ElasticError::OutputFrameLimit {
                frames: end / channels,
                limit: scratch.capacity() / channels,
            });
        }
        scratch
            .ensure_len(end)
            .map_err(|_| ElasticError::PoolCapacity)?;
        let source = self
            .pending_source
            .as_deref()
            .ok_or(ElasticError::PoolCapacity)?;
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        if let Err(error) = engine.process(request, source, &mut scratch[start..end]) {
            scratch.truncate(start);
            return Err(error);
        }
        self.output_start_meta = self.pending_meta;
        self.pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .clear();
        self.pending_meta = None;
        self.output_remainder = 0.0;
        self.active = true;
        Ok(())
    }
}
