use std::{mem, num::NonZeroUsize, ops::ControlFlow};

use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, SampleCount};
use kithara_stretch::ElasticError;
use num_traits::ToPrimitive;
use tracing::warn;

use super::core::{PreparedQuantum, WarpRenderer};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn advance_transition(
        &mut self,
        channels: usize,
        replacement: Option<SampleBuffer>,
        output_limit: usize,
    ) -> Option<AudioChunk> {
        if !self.active {
            return self.emit_pending_unity(replacement, output_limit);
        }
        let complete = match self.drain_tail(channels, output_limit) {
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
            return self.emit_pending_unity(replacement, output_limit);
        }

        warn!("time-stretch transition tail stopped without output");
        self.retire_transition_tail(replacement);
        None
    }

    pub(super) fn begin_unity_transition(
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
        self.render_terminal_pending(channels, 1)?;
        if self.active && self.output_start_meta.is_none() {
            self.output_start_meta = tail_start_meta;
        }
        self.queue_unity(meta, samples)
    }

    pub(in crate::render) fn drain_tail(
        &mut self,
        channels: usize,
        output_limit: usize,
    ) -> Result<bool, ElasticError> {
        if !self.active {
            return Ok(true);
        }
        let frame_limit = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .max_output_frames()
            .min(output_limit)
            .min(
                self.scratch
                    .as_ref()
                    .map_or(0, |scratch| scratch.capacity() / channels),
            );
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
        let engine = if self.mapped_render
            && self
                .projection
                .as_ref()
                .is_some_and(|projection| projection.stages > 1)
        {
            let projection = self.projection.as_mut().ok_or(ElasticError::PoolCapacity)?;
            projection.engines[projection.stages - 2].as_mut()
        } else {
            self.engine
                .as_mut()
                .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
                .as_mut()
        };
        let drain = engine.flush(&mut scratch[start..sample_limit])?;
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

    /// Assemble an output chunk from `scratch`, preserving the exact source
    /// start and the latest decoder frontier. `replacement` is retained for
    /// shell-side preparation before the next checked tick.
    ///
    /// A non-empty output always carries the live source spec, since the default metadata sentinel
    /// has zero channels and cannot reach the resampler.
    pub(super) fn emit(
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
        meta.source_span = None;
        self.record_rendered_source_end(meta, held_source_frames);
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

    pub(super) fn emit_pending_unity(
        &mut self,
        replacement: Option<SampleBuffer>,
        output_limit: usize,
    ) -> Option<AudioChunk> {
        let mut meta = self.pending_unity_meta?;
        let channels = usize::from(meta.spec.channels);
        let frames = usize::try_from(meta.frames).ok()?;
        if frames > output_limit {
            let count = output_limit.checked_mul(channels)?;
            let scratch = self.scratch.as_mut()?;
            if count > scratch.capacity() {
                return None;
            }
            scratch.ensure_len(count).ok()?;
            let pending = self.pending_source.as_mut()?;
            scratch.copy_from_slice(pending.get(..count)?);
            drop(pending.drain(..count));
            let mut next = Self::meta_at_frame(
                meta,
                meta.frame_offset
                    .checked_add(u64::try_from(output_limit).ok()?)?,
            );
            next.frames = u32::try_from(frames - output_limit).ok()?;
            self.pending_unity_meta = Some(next);
            meta.frames = u32::try_from(output_limit).ok()?;
            let samples = self.scratch.take()?;
            self.defer_scratch(replacement);
            return Some(AudioChunk::new(meta, samples));
        }
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

    pub(super) fn finish_transition_tail(&mut self) {
        self.reset_pending |= self.active;
        self.pending_meta = None;
        self.applied_pitch = f64::NAN;
        self.output_remainder = 0.0;
        self.source_frames_admitted = 0;
        self.primed_source_debt = 0;
        self.active = false;
        self.region = None;
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Render the quantum's source frames from the residency at the engine's
    /// feed; the chunk has already extended the residency.
    pub(super) fn process_active(
        &mut self,
        chunk: AudioChunk,
        speed: f32,
        prepared: Option<PreparedQuantum>,
    ) -> Option<AudioChunk> {
        if self.engine.is_none() || self.scratch.is_none() {
            warn!("time-stretch target was not prepared before rendering");
            self.defer_scratch(Some(chunk.samples));
            return None;
        }

        let AudioChunk { meta, samples } = chunk;
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }

        let channels = usize::from(self.spec.channels.max(1));
        let source_end = meta
            .frame_offset
            .saturating_add(u64::try_from(samples.len() / channels).unwrap_or(u64::MAX));
        let feed = self.resident_feed.unwrap_or(meta.frame_offset);
        let frames = prepared.map_or_else(
            || usize::try_from(source_end.saturating_sub(feed)).unwrap_or(usize::MAX),
            |quantum| quantum.active_frames,
        );
        if frames > self.source_block_frames.get() {
            let error = ElasticError::SourceFrameLimit {
                frames,
                limit: self.source_block_frames.get(),
            };
            warn!(%error, "time-stretch rendering failed; dropping chunk");
            self.defer_scratch(Some(samples));
            return None;
        }
        let mut input = Self::meta_at_frame(meta, feed);
        input.frames = u32::try_from(frames).unwrap_or(u32::MAX);
        self.last_input_meta = Some(input);
        let landing = prepared.and_then(|quantum| quantum.landing_frames);
        let residency = self.residency.take();
        let rendered = residency
            .as_ref()
            .ok_or(ElasticError::PoolCapacity)
            .and_then(|resident| {
                let end = feed
                    .checked_add(
                        u64::try_from(frames).map_err(|_| ElasticError::SampleCountOverflow)?,
                    )
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let range = i64::try_from(feed)
                    .map_err(|_| ElasticError::SampleCountOverflow)
                    .and_then(|feed| resident.range(feed, end, channels))?;
                self.render_active(
                    input,
                    &resident.samples[range],
                    speed,
                    channels,
                    frames,
                    landing,
                )
            });
        self.residency = residency;
        let rendered =
            rendered.and_then(
                |()| match (self.residency.as_mut(), self.scratch.as_mut()) {
                    (Some(resident), Some(output)) => resident.blend_replacement(output, channels),
                    _ => Ok(()),
                },
            );
        if let Err(error) = rendered {
            warn!(%error, "time-stretch rendering failed; dropping chunk");
            self.retire_engine();
            self.clear_render_state();
            self.defer_scratch(Some(samples));
            return None;
        }
        self.source_frames_admitted = self
            .source_frames_admitted
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        let next = feed.saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        self.resident_feed = (next < source_end).then_some(next);
        let held_source_frames = self.held_source_frames();
        self.emit(Some(samples), held_source_frames)
    }

    pub(super) fn process_unity(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let channels = usize::from(self.spec.channels.max(1));
        if !self.active && self.pending_frames(channels) == 0 {
            if let Some(resident) = self.residency.as_mut()
                && let Err(error) = resident.blend_replacement(&mut chunk.samples, channels)
            {
                warn!(%error, "time-stretch passthrough crossfade failed");
                return None;
            }
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

        self.advance_transition(channels, Some(samples), usize::MAX)
    }

    pub(super) fn queue_unity(
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

    pub(super) fn retire_transition_tail(&mut self, replacement: Option<SampleBuffer>) {
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
        self.primed_source_debt = 0;
        self.reset_pending = false;
        self.active = false;
        self.region = None;
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    fn drain_mapped(&mut self, output_limit: usize) -> Result<Option<AudioChunk>, ElasticError> {
        if !self.active && self.retiring_target.is_none() {
            return Ok(None);
        }
        let Some(meta) = self.last_input_meta else {
            return Ok(None);
        };
        let end = meta
            .frame_offset
            .checked_add(u64::from(meta.frames))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let end = *self.terminal_source_end.get_or_insert(end);
        let capability = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .max_output_frames();
        let quantum = self
            .render_quantum_frames
            .map_or(Self::MAX_OUTPUT_FRAMES, NonZeroUsize::get)
            .min(output_limit)
            .min(self.source_block_frames.get())
            .min(capability);
        let span = self.mapping_span(meta.frame_offset, meta.spec.sample_rate, quantum)?;
        let mut outputs = span.output_frames();
        while outputs > 0 {
            let (numerator, denominator) = span
                .source_ratio_at(outputs - 1)
                .ok_or(ElasticError::SampleCountOverflow)?;
            if numerator / denominator.get() < u128::from(end) {
                break;
            }
            outputs -= 1;
        }
        if outputs == 0 {
            self.active = false;
            self.reset_pending = true;
            self.prepared_quantum = None;
            self.clear_pending_source();
            self.trajectory.reset();
            return Ok(None);
        }
        let span = span
            .for_output_range(0..outputs)
            .ok_or(ElasticError::SampleCountOverflow)?;
        self.render_mapped(span)?;
        let channels = usize::from(self.spec.channels);
        self.residency
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .blend_replacement(
                self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?,
                channels,
            )?;
        let mut output = self.emit(None, 0).ok_or(ElasticError::EmptyOutput)?;
        output.meta.render_revision = self.rate.revision();
        output.meta.source_span = Some(span);
        self.map_output(&mut output)?;
        self.commit_render(self.context.load(), &output);
        Ok(Some(output))
    }

    fn finish_flush(
        &mut self,
        output: Option<AudioChunk>,
        complete: bool,
        snapshot: Option<crate::RenderSnapshot>,
    ) -> Option<AudioChunk> {
        let output = match output {
            Some(output) if output.frames() == 0 => {
                self.defer_scratch(Some(output.samples));
                None
            }
            output => output,
        };
        if let Some(output) = output.as_ref() {
            self.commit_render(snapshot, output);
        }
        if complete {
            self.backend_transition_pending = false;
            self.reprime_pending = false;
            self.active = false;
            self.pending_meta = None;
            self.source_frames_admitted = 0;
            self.primed_source_debt = 0;
            self.reset_pending = true;
        }
        output
    }

    /// Drain one buffered output chunk after source EOF or a transition.
    /// `output_limit` is an upper bound, further bounded by the backend and
    /// pending output. Keep draining until `None` to consume the entire tail.
    ///
    /// # Errors
    /// Returns a service request when the engine needs re-priming, or its
    /// admission error if the requested bounded run cannot be rendered.
    pub fn drain(
        &mut self,
        output_limit: usize,
    ) -> Result<Option<AudioChunk>, crate::WarpRenderError> {
        let output_limit = self.trajectory.output_limit(output_limit);
        if output_limit == 0 {
            return Ok(None);
        }
        if self.mapped_render {
            if self.transition_pending() {
                return Err(crate::WarpRenderError::NeedsService);
            }
            return self.drain_mapped(output_limit).map_err(Into::into);
        }
        if !self.requires_staging() {
            return Ok(None);
        }
        let snapshot = self.context.load();
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        } else {
            warn!("time-stretch output scratch was not serviced before flush");
            return Err(crate::WarpRenderError::NeedsService);
        }
        self.output_start_meta = None;
        let channels = usize::from(self.spec.channels.max(1));
        if self.pending_unity_meta.is_some() {
            let output = self.advance_transition(channels, None, output_limit);
            if let Some(output) = output.as_ref() {
                self.commit_render(snapshot, output);
            }
            return Ok(output);
        }
        let result = self
            .render_terminal_pending(channels, output_limit)
            .and_then(|()| {
                if self.pending_frames(channels) > 0 {
                    Ok(false)
                } else {
                    self.drain_tail(channels, output_limit)
                }
            });
        let complete = match result {
            Ok(complete) => complete,
            Err(error) => {
                warn!(%error, "time-stretch engine flush failed");
                self.retire_engine();
                self.clear_render_state();
                return Err(error.into());
            }
        };
        let held_source_frames = if complete {
            0
        } else {
            self.held_source_frames()
        };
        let output = self.emit(None, held_source_frames);
        Ok(self.finish_flush(output, complete, snapshot))
    }

    /// Prepare deferred renderer state for the current source format.
    pub fn prepare(&mut self, spec: AudioSpec) {
        self.service_target(spec);
    }

    /// Render one complete decoded source chunk.
    ///
    /// Returns the original input when it requires splitting into prepared
    /// quanta. The caller retains the unconsumed suffix between operations.
    /// Curves require the output-indexed `prepare_quantum` / `render_quantum` path.
    pub fn render(&mut self, mut chunk: AudioChunk) -> ControlFlow<AudioChunk, Option<AudioChunk>> {
        if self.transition_pending() || self.engine_outdated() || self.trajectory.requires_quanta()
        {
            return ControlFlow::Break(chunk);
        }
        if !self.requires_staging() && self.plan.is_some() {
            return ControlFlow::Break(chunk);
        }
        let snapshot = self.context.load();
        self.prepared_quantum = None;
        let rate = self.rate;
        let speed = rate.speed();
        chunk.meta.render_revision = rate.revision();
        ControlFlow::Continue(self.render_at(chunk, speed, snapshot, None))
    }

    fn render_at(
        &mut self,
        chunk: AudioChunk,
        speed: f32,
        snapshot: Option<crate::RenderSnapshot>,
        prepared: Option<PreparedQuantum>,
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

        let mut output = self.render_manual(chunk, speed, prepared);
        if prepared.is_some_and(|quantum| quantum.source_span.is_some())
            && let Some(output) = output.as_mut()
            && let Err(error) = self.map_output(output)
        {
            warn!(%error, "source mapping failed");
            return None;
        }
        if let Some(output) = output.as_ref() {
            self.commit_rate_render(snapshot, output, speed);
        }
        output
    }

    fn render_manual(
        &mut self,
        chunk: AudioChunk,
        speed: f32,
        prepared: Option<PreparedQuantum>,
    ) -> Option<AudioChunk> {
        if let Some(residency) = self.residency.as_mut()
            && let Err(error) = residency.retain_manual(
                chunk.meta,
                &chunk.samples,
                self.rendered_source_end.map(|(source, _)| source),
            )
        {
            warn!(%error, "source history retention failed");
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        if let Some(span) = prepared.and_then(|quantum| quantum.source_span) {
            self.mapped_render = true;
            let channels = usize::from(self.spec.channels);
            if span.output_frames() == 0 {
                self.last_input_meta = Some(chunk.meta);
                self.defer_scratch(Some(chunk.samples));
                return None;
            }
            if self.plan.is_none()
                && (!self.requires_staging() || self.trajectory.unity_interval())
                && self.retiring_target.is_none()
                && span.source_ratio_at(0)
                    == Some((
                        u128::from(chunk.meta.frame_offset),
                        std::num::NonZeroU128::MIN,
                    ))
                && usize::try_from(span.output_frames()).ok() == Some(chunk.frames())
            {
                self.last_input_meta = Some(chunk.meta);
                let mut output = self.process_unity(chunk)?;
                output.meta.source_span = Some(span);
                return Some(output);
            }
            let result = self.render_mapped(span).and_then(|()| {
                let resident = self.residency.as_mut().ok_or(ElasticError::PoolCapacity)?;
                let scratch = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
                resident.blend_replacement(scratch, channels)
            });
            return match result {
                Ok(()) => {
                    self.last_input_meta = Some(chunk.meta);
                    let mut output = self.emit(Some(chunk.samples), 0)?;
                    output.meta.source_span = Some(span);
                    Some(output)
                }
                Err(error) => {
                    warn!(%error, "varispeed source mapping failed");
                    if let Some(scratch) = self.scratch.as_mut() {
                        scratch.clear();
                    }
                    self.defer_scratch(Some(chunk.samples));
                    None
                }
            };
        }
        if self.unity_passthrough(speed) {
            return self.process_unity(chunk);
        }
        if let Some(prepared) = prepared
            && let Err(error) = self.activate_prepared_quantum(&chunk, prepared)
        {
            warn!(%error, "time-stretch activation failed; dropping chunk");
            self.retire_engine();
            self.clear_render_state();
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        self.process_active(chunk, speed, prepared)
    }

    /// Render the source span selected by [`Self::prepare_quantum`].
    ///
    /// Returns the unchanged input if no matching quantum was prepared.
    pub fn render_quantum(
        &mut self,
        mut chunk: AudioChunk,
    ) -> ControlFlow<AudioChunk, Option<AudioChunk>> {
        let Some(prepared) = self.prepared_quantum else {
            return ControlFlow::Break(chunk);
        };
        if chunk.frames() != prepared.frames
            || chunk.meta.frame_offset != prepared.source_start
            || chunk.spec() != self.spec
            || self.transition_pending()
        {
            return ControlFlow::Break(chunk);
        }
        self.prepared_quantum = None;
        let snapshot = self.context.load();
        chunk.meta.render_revision = prepared.rate.revision();
        ControlFlow::Continue(self.render_at(chunk, prepared.speed, snapshot, Some(prepared)))
    }

    /// Discard renderer state after a source discontinuity.
    pub fn reset(&mut self) {
        self.reset_pending = true;
        self.clear_render_state();
        self.committed = None;
        self.prepared_quantum = None;
    }
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    /// Retire the running engine's tail into the replacement the re-primed
    /// engine fades from, leaving the held source in the residency.
    pub(in crate::render) fn retire_for_reprime(&mut self) -> Result<(), ElasticError> {
        let speed_reprime = self.reprime_pending
            && self.current_keylock
            && !self.keylocked_unity()
            && self.stretch_target() == (self.current_kind, self.current_keylock)
            && self.retiring_target.is_none();
        if self.mapped_render && !speed_reprime {
            self.retire_mapped_target()?;
            let resident = self.residency.as_mut().ok_or(ElasticError::PoolCapacity)?;
            resident.replacement.clear();
            resident.next_replacement.clear();
            resident.replacement_offset = 0;
            self.output_remainder = 0.0;
            self.resident_feed = None;
            self.backend_transition_pending = false;
            self.reprime_pending = false;
            self.active = false;
            self.applied_pitch = f64::NAN;
            self.rebuild_pending = true;
            self.reset_pending = false;
            return Ok(());
        }
        match self.retain_replacement() {
            Ok(()) => {
                if let Some(pending) = self.pending_source.as_mut() {
                    pending.clear();
                }
                self.pending_meta = None;
                self.output_remainder = 0.0;
                self.resident_feed = None;
                self.rebuild_pending |= self.mapped_render && speed_reprime;
                Ok(())
            }
            Err(error) => {
                self.retire_engine();
                self.clear_render_state();
                Err(error)
            }
        }
    }

    /// Drain the running engine's tail into the replacement the next engine
    /// fades from. A fade still under way blends into that tail on the way, so
    /// the next fade starts from what sounds now.
    fn retain_replacement(&mut self) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels.max(1));
        let resident = self.residency.as_mut().ok_or(ElasticError::PoolCapacity)?;
        resident.next_replacement.clear();
        let capacity = resident.next_replacement.capacity() / channels;
        let quantum = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency()
            .second()
            .max(1);
        for _ in 0..=capacity.div_ceil(quantum) {
            self.scratch
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?
                .clear();
            let complete = self.drain_tail(channels, quantum)?;
            let resident = self.residency.as_mut().ok_or(ElasticError::PoolCapacity)?;
            let output = self.scratch.as_mut().ok_or(ElasticError::PoolCapacity)?;
            if resident.next_replacement.len() + output.len() > resident.next_replacement.capacity()
            {
                return Err(ElasticError::PoolCapacity);
            }
            resident.blend_replacement(output, channels)?;
            resident
                .next_replacement
                .try_extend_from_slice(output)
                .map_err(|_| ElasticError::PoolCapacity)?;
            output.clear();
            if complete {
                mem::swap(&mut resident.replacement, &mut resident.next_replacement);
                resident.next_replacement.clear();
                resident.replacement_offset = 0;
                self.backend_transition_pending = false;
                self.reprime_pending = false;
                self.active = false;
                self.applied_pitch = f64::NAN;
                self.reset_pending = false;
                return Ok(());
            }
        }
        Err(ElasticError::EnginePreparation(
            "backend drain exceeds its capability bound",
        ))
    }
}
