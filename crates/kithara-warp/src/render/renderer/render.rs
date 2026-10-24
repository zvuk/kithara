use std::ops::Range;

use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunkInfo, FrameCount, SampleCount, SourceSpan};
use kithara_stretch::{ElasticCapabilities, ElasticError, ElasticRequest};
use num_traits::ToPrimitive;

use super::core::WarpRenderer;
use crate::render::trajectory::Fraction;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(in crate::render) fn render_varispeed(
        &mut self,
        span: SourceSpan,
    ) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels);
        let frames =
            usize::try_from(span.output_frames()).map_err(|_| ElasticError::SampleCountOverflow)?;
        let samples = frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
        if samples > scratch.capacity() {
            return Err(ElasticError::PoolCapacity);
        }
        scratch
            .ensure_len(samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        scratch.truncate(samples);
        let resident = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let mut position = span.source_ratio_at(0);
        for frame in 0..frames {
            let (numerator, denominator) = position.ok_or(ElasticError::SampleCountOverflow)?;
            let endpoint = u64::try_from(frame)
                .ok()
                .and_then(|frame| frame.checked_add(1))
                .and_then(|frame| span.source_ratio_at(frame))
                .ok_or(ElasticError::SampleCountOverflow)?;
            let speed = Fraction {
                numerator: endpoint.0,
                denominator: endpoint.1,
            }
            .sub(Fraction {
                numerator,
                denominator,
            })
            .and_then(Fraction::as_f64)
            .ok_or(ElasticError::SampleCountOverflow)?;
            position = Some(endpoint);
            super::super::source_sample::source_frame(
                resident,
                (numerator, denominator),
                speed,
                self.terminal_source_end,
                &mut scratch[frame * channels..(frame + 1) * channels],
            )?;
        }
        Ok(())
    }
}

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
        if self.pending_meta.is_none() {
            self.pending_meta = Some(Self::meta_at_frame(meta, frame_offset)?);
        }
        Ok(())
    }

    pub(in crate::render) fn balanced_source_block(remaining: usize, limit: usize) -> usize {
        let partitions = remaining.div_ceil(limit);
        remaining.div_ceil(partitions)
    }

    /// Backends require a non-empty output, so a sub-frame source span stays pending until its
    /// cumulative exact output reaches one full frame; EOF rounds the final residual once.
    pub(in crate::render) fn output_frames(
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
        let output_frames = if exact < 1.0 { 0.0 } else { exact.round() };
        let output_frames = output_frames
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        let emitted = output_frames
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok((output_frames, exact - emitted))
    }

    fn quantized_source_span(
        frames: usize,
        pending_frames: usize,
        stretch: f64,
        remainder: f64,
        capabilities: ElasticCapabilities,
        output_limit: usize,
    ) -> Result<Option<usize>, ElasticError> {
        let envelope = capabilities.rate_envelope();
        let mut source = frames;
        while source > 0 {
            let (output, _) = Self::output_frames(source, stretch, remainder)?;
            if output == 0 {
                return Ok(Some(source));
            }
            let total_source = pending_frames
                .checked_add(source)
                .and_then(|frames| frames.to_f64())
                .ok_or(ElasticError::SampleCountOverflow)?;
            let output_f = output.to_f64().ok_or(ElasticError::SampleCountOverflow)?;
            let rate = total_source / output_f;
            if output <= output_limit && envelope.contains_rate(rate) {
                return Ok(Some(source));
            }
            let next = if output <= output_limit && rate > envelope.max_source_frames_per_output() {
                (output_f * envelope.max_source_frames_per_output())
                    .floor()
                    .to_usize()
                    .ok_or(ElasticError::SampleCountOverflow)?
                    .saturating_sub(pending_frames)
            } else {
                let next_output = output.saturating_sub(1).min(output_limit);
                let rounding_boundary = next_output
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?
                    + 0.5;
                ((rounding_boundary - remainder) / stretch)
                    .ceil()
                    .max(0.0)
                    .to_usize()
                    .ok_or(ElasticError::SampleCountOverflow)?
                    .saturating_sub(1)
            };
            source = next.min(source - 1);
        }
        Ok(None)
    }

    pub(in crate::render) fn source_block_limit(
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
}

struct ActiveBlock {
    meta: AudioChunkInfo,
    frame: u64,
    channels: usize,
    pending_frames: usize,
    sub: usize,
    request_span: Option<usize>,
    output_frames: usize,
    next_remainder: f64,
}

impl ActiveBlock {
    fn defers(&self) -> bool {
        self.output_frames == 0 || self.request_span.is_none()
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(in crate::render) fn render_active(
        &mut self,
        meta: AudioChunkInfo,
        samples: &[f32],
        speed: f32,
        channels: usize,
        frames: usize,
    ) -> Result<(), ElasticError> {
        let base = 1.0 / f64::from(speed);
        let pitch = if self.current_keylock {
            1.0
        } else {
            f64::from(speed)
        };
        let mut consumed = 0usize;
        let mut frame = meta.frame_offset;
        self.apply_pitch(pitch)?;
        for _ in 0..frames {
            if consumed == frames {
                return Ok(());
            }
            let region = self.region_for(frame);
            let left = u64::try_from(frames - consumed).unwrap_or(u64::MAX);
            let span = region.end().saturating_sub(frame).min(left).max(1);
            let stretch = base * region.correction();
            let capabilities = self
                .engine
                .as_ref()
                .map(|engine| engine.capabilities())
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
            let remaining = usize::try_from(span).unwrap_or(frames - consumed);
            let pending_frames = self.pending_frames(channels);
            let available = Self::available_source(stretch, capabilities, pending_frames)?;
            let sub = Self::balanced_source_block(remaining, available);
            let request_span = Self::quantized_source_span(
                sub,
                pending_frames,
                stretch,
                self.output_remainder,
                capabilities,
                capabilities.max_output_frames(),
            )?;
            let sub = request_span.unwrap_or(sub);
            let (output_frames, next_remainder) =
                Self::output_frames(sub, stretch, self.output_remainder)?;
            let block = ActiveBlock {
                meta,
                frame,
                channels,
                pending_frames,
                sub,
                request_span,
                output_frames,
                next_remainder,
            };
            let part = &samples[consumed * channels..(consumed + sub) * channels];
            if block.defers() {
                self.defer_pending_source(part, &block)?;
            } else {
                self.process_block(part, &block, capabilities.max_output_frames())?;
            }
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

    fn start_active_output(
        &mut self,
        meta: AudioChunkInfo,
        frame: u64,
        pending_frames: usize,
        start: usize,
    ) -> Result<(), ElasticError> {
        if start == 0 && self.output_start_meta.is_none() {
            self.output_start_meta = if pending_frames > 0 {
                self.pending_meta
            } else {
                Some(Self::meta_at_frame(meta, frame)?)
            };
        }
        Ok(())
    }

    fn terminal_next_meta(
        &self,
        admitted_source_frames: usize,
        source_frames: usize,
    ) -> Result<Option<AudioChunkInfo>, ElasticError> {
        let next_meta = if admitted_source_frames == source_frames {
            None
        } else {
            let meta = self.pending_meta.ok_or(ElasticError::EmptySource)?;
            let next = meta
                .frame_offset
                .checked_add(
                    u64::try_from(admitted_source_frames)
                        .map_err(|_| ElasticError::SampleCountOverflow)?,
                )
                .ok_or(ElasticError::SampleCountOverflow)?;
            Some(Self::meta_at_frame(meta, next)?)
        };
        Ok(next_meta)
    }

    fn process_terminal_output(
        &mut self,
        request: ElasticRequest,
        admitted_samples: usize,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let output_samples = request
            .output_frames()
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
        if let Err(error) = engine.process(
            request,
            &source[..admitted_samples],
            &mut scratch[start..end],
        ) {
            scratch.truncate(start);
            return Err(error);
        }
        Ok(())
    }

    pub(in crate::render) fn render_terminal_pending(
        &mut self,
        channels: usize,
        output_limit: usize,
    ) -> Result<(), ElasticError> {
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

        let total_output_frames = output_frames;
        let capability = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .max_output_frames();
        let output_frames = FrameCount::new(output_frames.min(output_limit).min(capability));
        let admitted_source_frames = if output_frames.get() == total_output_frames {
            source_frames
        } else {
            source_frames
                .checked_mul(output_frames.get())
                .and_then(|frames| frames.checked_div(total_output_frames))
                .filter(|frames| *frames > 0)
                .ok_or(ElasticError::EmptySource)?
        };
        let request = ElasticRequest::new(admitted_source_frames, output_frames.get())?;
        let admitted_samples = admitted_source_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let next_meta = self.terminal_next_meta(admitted_source_frames, source_frames)?;
        self.process_terminal_output(request, admitted_samples, channels)?;
        self.output_start_meta = self.pending_meta;
        drop(
            self.pending_source
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?
                .drain(..admitted_samples),
        );
        self.pending_meta = next_meta;
        self.output_remainder -= output_frames
            .get()
            .to_f64()
            .ok_or(ElasticError::SampleCountOverflow)?;
        if admitted_source_frames == source_frames {
            self.output_remainder = 0.0;
        }
        self.active = true;
        Ok(())
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    fn available_source(
        stretch: f64,
        capabilities: ElasticCapabilities,
        pending_frames: usize,
    ) -> Result<usize, ElasticError> {
        let source_limit =
            Self::source_block_limit(stretch, capabilities, capabilities.max_output_frames())?;
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
        Ok(available)
    }

    fn block_output(&self, block: &ActiveBlock) -> Result<Range<usize>, ElasticError> {
        let output_samples = block
            .output_frames
            .checked_mul(block.channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let start = self.scratch.as_deref().map_or(0, <[f32]>::len);
        let end = start
            .checked_add(output_samples)
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(start..end)
    }

    fn block_request(
        block: &ActiveBlock,
        max_output_frames: usize,
    ) -> Result<ElasticRequest, ElasticError> {
        if block.output_frames > max_output_frames {
            return Err(ElasticError::OutputFrameLimit {
                frames: block.output_frames,
                limit: max_output_frames,
            });
        }
        let source_frames = block
            .pending_frames
            .checked_add(block.sub)
            .ok_or(ElasticError::SampleCountOverflow)?;
        ElasticRequest::new(source_frames, block.output_frames)
    }

    fn defer_pending_source(
        &mut self,
        part: &[f32],
        block: &ActiveBlock,
    ) -> Result<(), ElasticError> {
        self.append_pending_source(part, block.meta, block.frame)?;
        self.output_remainder = block.next_remainder
            + block
                .output_frames
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(())
    }

    fn process_block(
        &mut self,
        part: &[f32],
        block: &ActiveBlock,
        max_output_frames: usize,
    ) -> Result<(), ElasticError> {
        let request = Self::block_request(block, max_output_frames)?;
        let output = self.block_output(block)?;
        if block.pending_frames > 0 {
            self.append_pending_source(part, block.meta, block.frame)?;
        }
        self.start_active_output(block.meta, block.frame, block.pending_frames, output.start)?;
        self.render_block(part, block, request, output)?;
        if block.pending_frames > 0 {
            self.clear_pending_source();
        }
        self.output_remainder = block.next_remainder;
        self.active = true;
        Ok(())
    }

    fn render_block(
        &mut self,
        part: &[f32],
        block: &ActiveBlock,
        request: ElasticRequest,
        Range { start, end }: Range<usize>,
    ) -> Result<(), ElasticError> {
        let scratch = self
            .scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "output scratch is unavailable",
            ))?;
        if end > scratch.capacity() {
            return Err(ElasticError::OutputFrameLimit {
                frames: end / block.channels,
                limit: scratch.capacity() / block.channels,
            });
        }
        scratch
            .ensure_len(end)
            .map_err(|_| ElasticError::PoolCapacity)?;
        let source = self
            .pending_source
            .as_deref()
            .filter(|_| block.pending_frames > 0)
            .unwrap_or(part);
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        if let Err(error) = engine.process(request, source, &mut scratch[start..end]) {
            scratch.truncate(start);
            return Err(error);
        }
        Ok(())
    }
}
