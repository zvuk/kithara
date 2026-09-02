use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunkInfo, FrameCount, SampleCount};
use kithara_stretch::{ElasticError, ElasticRequest};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::renderer::WarpRenderer;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn apply_pitch(&mut self, pitch: f64) -> Result<(), ElasticError> {
        if !self.applied_pitch.is_nan()
            && (pitch - self.applied_pitch).abs() <= Self::PITCH_UPDATE_EPSILON
        {
            return Ok(());
        }
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        engine.set_pitch(pitch)?;
        self.applied_pitch = pitch;
        Ok(())
    }

    #[kithara::measure]
    pub(super) fn process_request(
        &mut self,
        request: ElasticRequest,
        direct_source: &[f32],
        channels: usize,
        use_pending_source: bool,
    ) -> Result<(), ElasticError> {
        let output_samples = request
            .output_frames()
            .checked_mul(channels)
            .map(SampleCount::new)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self
            .scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "output scratch is unavailable",
            ))?;
        let start = scratch.len();
        let end = start
            .checked_add(output_samples.get())
            .ok_or(ElasticError::SampleCountOverflow)?;
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
            .filter(|_| use_pending_source)
            .unwrap_or(direct_source);
        let engine = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        if let Err(error) = engine.process(request, source, &mut scratch[start..end]) {
            scratch.truncate(start);
            return Err(error);
        }
        self.active = true;
        Ok(())
    }

    #[kithara::measure]
    pub(super) fn render_active(
        &mut self,
        meta: AudioChunkInfo,
        samples: &[f32],
        speed: f32,
        channels: usize,
        frames: usize,
    ) -> Result<(), ElasticError> {
        let base = 1.0 / f64::from(speed);
        let pitch = if self.controls.keylock() {
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
            let source_limit = Self::source_block_limit(
                stretch,
                capabilities.max_source_frames(),
                capabilities.max_output_frames(),
            )?;
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
            let output_frames = FrameCount::new(output_frames);
            let source_frames_per_output = source_frames
                .to_f64()
                .ok_or(ElasticError::SampleCountOverflow)?
                / output_frames
                    .get()
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?;
            if !capabilities
                .rate_envelope()
                .contains_rate(source_frames_per_output)
            {
                self.append_pending_source(part, meta, frame)?;
                self.output_remainder = next_remainder
                    + output_frames
                        .get()
                        .to_f64()
                        .ok_or(ElasticError::SampleCountOverflow)?;
                consumed += sub;
                frame = frame.saturating_add(
                    u64::try_from(sub).map_err(|_| ElasticError::SampleCountOverflow)?,
                );
                continue;
            }
            let request = ElasticRequest::new(source_frames, output_frames.get())?;
            let start = self.scratch.as_deref().map_or(0, <[f32]>::len);
            if pending_frames > 0 {
                self.append_pending_source(part, meta, frame)?;
            }
            if start == 0 {
                self.output_start_meta = if pending_frames > 0 {
                    self.pending_meta
                } else {
                    Some(Self::meta_at_frame(meta, frame))
                };
            }
            self.process_request(request, part, channels, pending_frames > 0)?;
            if pending_frames > 0 {
                self.clear_pending_source();
            }
            self.output_remainder = next_remainder;
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
}
