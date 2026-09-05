use std::mem;

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, SampleCount};
use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;
use tracing::warn;

use super::renderer::WarpRenderer;

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Assemble an output chunk from `scratch`, preserving the exact source
    /// start and the latest decoder frontier. `replacement` is retained for
    /// shell-side preparation before the next checked tick.
    fn emit(
        &mut self,
        replacement: Option<SampleBuffer>,
        held_source_frames: u64,
    ) -> Option<AudioChunk> {
        let total = self.scratch.as_deref().map_or(0, <[f32]>::len);
        if total == 0 {
            self.defer_scratch(replacement);
            return None;
        }
        let frames = match self.spec.frame_count(SampleCount::new(total)) {
            Ok(frames) => frames,
            Err(error) => {
                warn!(?error, total, "discarding malformed Warp output shape");
                self.scratch.take();
                self.defer_scratch(replacement);
                return None;
            }
        };
        let mut meta = self.last_input_meta.unwrap_or_default();
        self.record_rendered_source_end(meta, held_source_frames);
        // A non-empty output always carries the live source spec. The default
        // metadata sentinel has zero channels and cannot reach the resampler.
        meta.spec = self.spec;
        meta.frames = u32::try_from(frames.get()).unwrap_or(u32::MAX);
        if let Some(start) = self.output_start_meta.take() {
            if start.frame_offset != meta.frame_offset {
                meta.source_byte_offset = None;
                meta.source_bytes = 0;
            }
            meta.frame_offset = start.frame_offset;
            meta.timestamp = start.timestamp;
        }
        let samples = self.scratch.take()?;
        self.defer_scratch(replacement);
        Some(AudioChunk::new(meta, samples))
    }

    fn finish_transition_tail(&mut self) {
        self.reset_pending |= self.active;
        self.pending_meta = None;
        self.applied_pitch = f64::NAN;
        self.output_remainder = 0.0;
        self.source_frames_admitted = 0;
        self.active = false;
        self.region = None;
    }

    fn retire_transition_tail(&mut self, replacement: Option<SampleBuffer>) {
        self.retire_engine();
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }
        self.defer_scratch(replacement);
        self.pending_meta = None;
        self.output_start_meta = None;
        self.applied_pitch = f64::NAN;
        self.output_remainder = 0.0;
        self.source_frames_admitted = 0;
        self.reset_pending = false;
        self.active = false;
        self.region = None;
    }

    fn queue_unity(
        &mut self,
        meta: AudioChunkInfo,
        samples: &mut SampleBuffer,
    ) -> Result<(), ElasticError> {
        let pending = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        if !pending.is_empty() {
            return Err(ElasticError::EnginePreparation(
                "time-stretch pending source was not committed before unity",
            ));
        }
        mem::swap(pending, samples);
        self.pending_unity_meta = Some(meta);
        Ok(())
    }

    fn begin_unity_transition(
        &mut self,
        meta: AudioChunkInfo,
        samples: &mut SampleBuffer,
        channels: usize,
    ) -> Result<(), ElasticError> {
        let tail_start_meta = self.last_input_meta;
        self.output_start_meta = None;
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }
        let rounded = self
            .output_remainder
            .round()
            .max(0.0)
            .to_usize()
            .ok_or(ElasticError::SampleCountOverflow)?;
        if rounded > 1 {
            return Err(ElasticError::OutputFrameLimit {
                frames: rounded,
                limit: 1,
            });
        }
        self.render_terminal_pending(channels)?;
        if self.active && self.output_start_meta.is_none() {
            self.output_start_meta = tail_start_meta;
        }
        self.queue_unity(meta, samples)
    }

    fn emit_pending_unity(&mut self, replacement: Option<SampleBuffer>) -> Option<AudioChunk> {
        let meta = self.pending_unity_meta?;
        let replacement = replacement
            .or_else(|| self.scratch.take())
            .or_else(|| self.deferred_scratch.take());
        let Some(mut replacement) = replacement else {
            warn!("time-stretch queued unity has no reusable buffer");
            return None;
        };
        replacement.clear();
        let Some(samples) = self.pending_source.take() else {
            self.pending_source = Some(replacement);
            warn!("time-stretch queued unity buffer is unavailable");
            return None;
        };
        self.pending_source = Some(replacement);
        self.pending_unity_meta = None;
        self.pending_meta = None;
        self.last_input_meta = Some(meta);
        self.output_start_meta = None;
        self.record_rendered_source_end(meta, 0);
        Some(AudioChunk::new(meta, samples))
    }

    fn drain_tail(&mut self, channels: usize) -> Result<bool, ElasticError> {
        if !self.active {
            return Ok(true);
        }
        let frame_limit = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency()
            .output_frames();
        let sample_limit = frame_limit
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let scratch = self
            .scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "output scratch is unavailable",
            ))?;
        let start = scratch.len();
        if start >= sample_limit {
            return Ok(false);
        }
        scratch
            .ensure_len(sample_limit)
            .map_err(|_| ElasticError::PoolCapacity)?;
        let drain = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .flush(&mut scratch[start..sample_limit])?;
        let rendered_frames = FrameCount::new(drain.frames());
        let available_frames = (sample_limit - start) / channels;
        if rendered_frames.get() > available_frames {
            return Err(ElasticError::EngineOutputFrameCount {
                actual: rendered_frames.get(),
                expected: available_frames,
            });
        }
        let rendered_samples = rendered_frames
            .get()
            .checked_mul(channels)
            .map(SampleCount::new)
            .ok_or(ElasticError::SampleCountOverflow)?;
        scratch.truncate(start + rendered_samples.get());
        if !drain.complete() && rendered_frames.get() == 0 {
            return Err(ElasticError::EnginePreparation(
                "time-stretch terminal drain stopped advancing",
            ));
        }
        Ok(drain.complete())
    }

    fn process_unity(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        let channels = usize::from(self.spec.channels.max(1));
        if !self.active && self.pending_frames(channels) == 0 {
            self.record_rendered_source_end(chunk.meta, 0);
            return Some(chunk);
        }

        let AudioChunk { meta, mut samples } = chunk;
        if let Err(error) = self.begin_unity_transition(meta, &mut samples, channels) {
            warn!(%error, "time-stretch transition to passthrough failed; dropping chunk");
            self.retire_engine();
            self.clear_render_state();
            self.defer_scratch(Some(samples));
            return None;
        }

        self.advance_transition(channels, Some(samples))
    }

    fn advance_transition(
        &mut self,
        channels: usize,
        replacement: Option<SampleBuffer>,
    ) -> Option<AudioChunk> {
        if !self.active {
            return self.emit_pending_unity(replacement);
        }
        let complete = match self.drain_tail(channels) {
            Ok(complete) => complete,
            Err(error) => {
                warn!(%error, "time-stretch transition tail failed; preserving queued unity");
                self.retire_transition_tail(replacement);
                return None;
            }
        };
        if complete {
            self.finish_transition_tail();
        }
        let held_source_frames = if complete {
            0
        } else {
            self.held_source_frames()
        };
        if self
            .scratch
            .as_deref()
            .is_some_and(|scratch| !scratch.is_empty())
        {
            return self.emit(replacement, held_source_frames);
        }
        if complete {
            return self.emit_pending_unity(replacement);
        }

        warn!("time-stretch transition tail stopped without output");
        self.retire_transition_tail(replacement);
        None
    }

    fn process_active(&mut self, chunk: AudioChunk, speed: f32) -> Option<AudioChunk> {
        if self.engine.is_none() || self.scratch.is_none() {
            warn!("time-stretch target was not prepared before rendering");
            self.defer_scratch(Some(chunk.samples));
            return None;
        }

        let AudioChunk { meta, samples } = chunk;
        self.last_input_meta = Some(meta);
        self.output_start_meta = None;
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }

        let channels = usize::from(self.spec.channels.max(1));
        let frames = samples.len() / channels;
        if frames > Self::MAX_SOURCE_FRAMES {
            let error = ElasticError::SourceFrameLimit {
                frames,
                limit: Self::MAX_SOURCE_FRAMES,
            };
            warn!(%error, "time-stretch rendering failed; dropping chunk");
            self.defer_scratch(Some(samples));
            return None;
        }
        if let Err(error) = self.render_active(meta, &samples, speed, channels, frames) {
            warn!(%error, "time-stretch rendering failed; dropping chunk");
            self.retire_engine();
            self.clear_render_state();
            self.defer_scratch(Some(samples));
            return None;
        }
        self.source_frames_admitted = self
            .source_frames_admitted
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        let held_source_frames = self.held_source_frames();
        self.emit(Some(samples), held_source_frames)
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Prepare deferred renderer state for the current source format.
    pub fn prepare(&mut self, spec: AudioSpec) {
        self.service_target(spec);
    }

    /// Drain one buffered output chunk after source EOF or a transition.
    pub fn flush(&mut self) -> Option<AudioChunk> {
        let snapshot = self.context.load();
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        } else {
            warn!("time-stretch output scratch was not serviced before flush");
            return None;
        }
        self.output_start_meta = None;
        let channels = usize::from(self.spec.channels.max(1));
        if self.transition_pending() {
            return self.advance_transition(channels, None);
        }
        let result = self
            .render_terminal_pending(channels)
            .and_then(|()| self.drain_tail(channels));
        let complete = match result {
            Ok(complete) => complete,
            Err(error) => {
                warn!(%error, "time-stretch engine flush failed");
                self.retire_engine();
                self.clear_render_state();
                return None;
            }
        };
        let held_source_frames = if complete {
            0
        } else {
            self.held_source_frames()
        };
        let output = self.emit(None, held_source_frames);
        if let Some(output) = output.as_ref() {
            self.commit_render(snapshot, output.frames());
        }
        output
    }

    /// Render one complete decoded source chunk.
    pub fn render(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let snapshot = self.context.load();
        self.prepared_quantum = None;
        let rate = self.controls.rate_target();
        chunk.meta.render_revision = rate.revision();
        self.render_at(chunk, rate.speed(), snapshot)
    }

    /// Render the source span selected by [`Self::prepare_quantum`].
    pub fn render_quantum(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let prepared = self.prepared_quantum.take()?;
        if chunk.frames() != prepared.frames {
            return None;
        }
        let snapshot = self.context.load();
        chunk.meta.render_revision = prepared.rate.revision();
        self.render_at(chunk, prepared.rate.speed(), snapshot)
    }

    fn render_at(
        &mut self,
        chunk: AudioChunk,
        speed: f32,
        snapshot: Option<crate::RenderSnapshot>,
    ) -> Option<AudioChunk> {
        if chunk.spec() != self.spec {
            warn!(
                expected = %self.spec,
                actual = %chunk.spec(),
                "time-stretch target was not serviced before a format change"
            );
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        if self.transition_pending() {
            warn!("time-stretch transition must drain before accepting new input");
            self.defer_scratch(Some(chunk.samples));
            return None;
        }

        let output = if self.unity_passthrough(speed) {
            self.process_unity(chunk)
        } else {
            self.process_active(chunk, speed)
        };
        if let Some(output) = output.as_ref() {
            self.commit_rate_render(
                snapshot,
                output.frames(),
                output.meta.render_revision,
                speed,
            );
        }
        output
    }

    /// Discard renderer state after a source discontinuity.
    pub fn reset(&mut self) {
        self.reset_pending = true;
        self.clear_render_state();
        self.committed = None;
    }
}
