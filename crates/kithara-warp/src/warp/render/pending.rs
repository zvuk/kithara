use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunk, AudioChunkInfo, FrameCount, SampleCount};
use kithara_stretch::{ElasticError, ElasticRequest};
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;
use tracing::warn;

use super::renderer::{PreparedActivation, PreparedQuantum, WarpRenderer};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    fn write_passthrough_history(history: &mut [f32], head: usize, source: &[f32]) -> usize {
        debug_assert!(!history.is_empty());
        debug_assert!(source.len() < history.len());
        let first = source.len().min(history.len() - head);
        history[head..head + first].copy_from_slice(&source[..first]);
        let rest = source.len() - first;
        history[..rest].copy_from_slice(&source[first..]);
        (head + source.len()) % history.len()
    }

    pub(super) fn retain_passthrough_history(
        &mut self,
        meta: AudioChunkInfo,
        source: &[f32],
    ) -> Result<(), ElasticError> {
        let Some(engine) = self.engine.as_ref() else {
            self.clear_pending_source();
            return Ok(());
        };
        let channels = usize::from(self.spec.channels.max(1));
        let history_frames = engine.capabilities().latency().source_frames();
        let history_samples = history_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let continuous = self
            .rendered_source_end
            .is_none_or(|(frame, sample_rate, _)| {
                frame == meta.frame_offset && sample_rate == meta.spec.sample_rate
            });
        if !continuous {
            self.clear_pending_source();
        }
        if history_samples == 0 {
            self.passthrough_history_head = Some(0);
            return Ok(());
        }
        let history = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        if history_samples > history.capacity() {
            return Err(ElasticError::SourceFrameLimit {
                frames: history_frames,
                limit: history.capacity() / channels,
            });
        }
        if source.len() >= history_samples {
            history
                .ensure_len(history_samples)
                .map_err(|_| ElasticError::PoolCapacity)?;
            history.copy_from_slice(&source[source.len() - history_samples..]);
            self.passthrough_history_head = Some(0);
            return Ok(());
        }

        let current = history.len();
        if current < history_samples {
            let appended = source.len().min(history_samples - current);
            history
                .try_extend_from_slice(&source[..appended])
                .map_err(|_| ElasticError::PoolCapacity)?;
            if appended == source.len() {
                self.passthrough_history_head = Some(0);
                return Ok(());
            }
            let rest = &source[appended..];
            self.passthrough_history_head = Some(Self::write_passthrough_history(history, 0, rest));
            return Ok(());
        }

        let head = self.passthrough_history_head.unwrap_or(0);
        self.passthrough_history_head =
            Some(Self::write_passthrough_history(history, head, source));
        Ok(())
    }

    pub(super) fn activation_latency_frames(&self) -> Option<(usize, usize)> {
        if self.active || self.scratch.is_none() || self.rendered_source_end.is_none() {
            return None;
        }
        let engine = self.engine.as_ref()?;
        let latency = engine.capabilities().latency();
        let history_frames = latency.source_frames();
        let channels = usize::from(self.spec.channels.max(1));
        let history_samples = history_frames.checked_mul(channels)?;
        if self.passthrough_history_head.is_none()
            || self.pending_source.as_deref()?.len() != history_samples
        {
            return None;
        }
        Some((history_frames, latency.output_frames()))
    }

    fn validate_prepared_activation(
        &self,
        prepared: &PreparedQuantum,
        activation: PreparedActivation,
    ) -> Result<(usize, usize), ElasticError> {
        let (history_frames, output_frames) =
            self.activation_latency_frames()
                .ok_or(ElasticError::EnginePreparation(
                    "Warp renderer activation context is unavailable",
                ))?;
        if history_frames != activation.history_frames
            || output_frames != activation.warm.output_frames()
        {
            return Err(ElasticError::EnginePreparation(
                "Warp renderer activation context changed",
            ));
        }
        let expected_warm = output_frames
            .to_f64()
            .map(|frames| (frames * f64::from(prepared.speed())).round())
            .and_then(|frames| frames.to_usize())
            .ok_or(ElasticError::SampleCountOverflow)?;
        if activation.warm.source_frames() != expected_warm {
            return Err(ElasticError::EnginePreparation(
                "Warp renderer activation rate changed",
            ));
        }
        Ok((history_frames, output_frames))
    }

    pub(super) fn activate_prepared_quantum(
        &mut self,
        chunk: &mut AudioChunk,
        prepared: &PreparedQuantum,
    ) -> Result<(), ElasticError> {
        let Some(activation) = prepared.activation() else {
            return Ok(());
        };
        let (history_frames, output_frames) =
            self.validate_prepared_activation(prepared, activation)?;
        let warm_frames = activation.warm.source_frames();
        let prefix_frames = activation.prefix_frames()?;
        let (cue, sample_rate, _) =
            self.rendered_source_end
                .ok_or(ElasticError::EnginePreparation(
                    "Warp renderer has no presented source frontier",
                ))?;
        if chunk.meta.frame_offset != cue || chunk.meta.spec.sample_rate != sample_rate {
            return Err(ElasticError::DiscontinuousSource {
                expected: cue.to_f64().ok_or(ElasticError::SampleCountOverflow)?,
                actual: chunk
                    .meta
                    .frame_offset
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            });
        }

        let active_frames = Self::prepared_source_frames(prepared)?;
        let total_frames = Self::prepared_input_frames(prepared)?;
        if chunk.frames() != total_frames {
            return Err(ElasticError::SourceFrameLimit {
                frames: chunk.frames(),
                limit: total_frames,
            });
        }
        let channels = usize::from(self.spec.channels.max(1));
        let history_samples = history_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let warm_samples = warm_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let discard_samples = output_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let prefix_samples = prefix_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let active_samples = active_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let active_end = prefix_samples
            .checked_add(active_samples)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let pitch = if self.controls.keylock() {
            1.0
        } else {
            f64::from(prepared.speed())
        };
        self.apply_pitch(pitch)?;
        let head = self
            .passthrough_history_head
            .ok_or(ElasticError::EnginePreparation(
                "Warp renderer history is unavailable",
            ))?;
        let history = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        if history.len() != history_samples {
            return Err(ElasticError::HistorySampleCount {
                actual: history.len(),
                expected: history_samples,
            });
        }
        history.rotate_left(head);
        self.passthrough_history_head = Some(0);

        let admitted =
            u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        let admitted = self
            .source_frames_admitted
            .checked_add(admitted)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let source_debt =
            u64::try_from(warm_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        let scratch = self
            .activation_scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "activation scratch is unavailable",
            ))?;
        scratch.clear();
        if discard_samples > scratch.capacity() {
            return Err(ElasticError::OutputFrameLimit {
                frames: output_frames,
                limit: scratch.capacity() / channels,
            });
        }

        let lookahead = chunk.samples.get(..history_samples).ok_or_else(|| {
            ElasticError::LookaheadSampleCount {
                actual: chunk.samples.len().min(history_samples),
                expected: history_samples,
            }
        })?;
        let warm = chunk
            .samples
            .get(history_samples..prefix_samples)
            .ok_or_else(|| ElasticError::SourceSampleCount {
                actual: chunk.samples.len().saturating_sub(history_samples),
                expected: warm_samples,
            })?;
        scratch
            .ensure_len(discard_samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        let result = self
            .engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .prime(activation.warm, history, lookahead, warm, scratch);
        scratch.clear();
        result?;

        self.clear_pending_source();
        chunk.samples.copy_within(prefix_samples..active_end, 0);
        chunk.samples.truncate(active_samples);
        let original = chunk.meta;
        let active_start = original
            .frame_offset
            .checked_add(
                u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        chunk.meta = Self::meta_at_frame(original, active_start);
        chunk.meta.frames =
            u32::try_from(active_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        chunk.meta.end_timestamp = original.end_timestamp;
        self.source_frames_admitted = admitted;
        self.primed_source_debt = source_debt;
        self.active = true;
        Ok(())
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn append_pending_source(
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

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn source_frames_for_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        speed: f32,
    ) -> Result<usize, ElasticError> {
        if remaining == 0 {
            return Err(ElasticError::EmptySource);
        }
        if self.can_passthrough(speed) {
            return Ok(remaining.min(self.render_quantum_frames.get()));
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
        let stretch = (1.0 / f64::from(speed)) * region.correction();
        let capabilities = self
            .engine
            .as_ref()
            .map(|engine| engine.capabilities())
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?;
        let output_limit = capabilities
            .max_output_frames()
            .min(self.render_quantum_frames.get());
        let source_limit =
            Self::source_block_limit(stretch, capabilities.max_source_frames(), output_limit)?;
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

    /// Return the next source span that fits the fixed output quantum.
    ///
    /// The playback scheduler uses this workspace-internal seam to prepare an
    /// owning pooled subchunk outside the checked render core.
    #[doc(hidden)]
    #[kithara::measure]
    pub fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
    ) -> Option<FrameCount> {
        match self.scheduler_plan(meta, remaining) {
            Ok(mut prepared) => match Self::prepared_input_frames(&prepared) {
                Ok(frames) => {
                    prepared.bind(self.context.load());
                    self.prepared_quantum = Some(prepared);
                    Some(FrameCount::new(frames))
                }
                Err(error) => {
                    self.prepared_quantum = None;
                    warn!(%error, "time-stretch source quantum sizing failed");
                    None
                }
            },
            Err(error) => {
                self.prepared_quantum = None;
                warn!(%error, "time-stretch source quantum sizing failed");
                None
            }
        }
    }

    /// Shrink the prepared source quantum at true source EOF without sampling
    /// newer controls or render context.
    #[doc(hidden)]
    pub fn prepare_terminal_quantum(
        &mut self,
        meta: AudioChunkInfo,
        frames: usize,
    ) -> Option<FrameCount> {
        if frames == 0 {
            self.prepared_quantum = None;
            return None;
        }
        let original = self.prepared_quantum.take();
        let (rate, snapshot, activation) = match original.as_ref() {
            Some(prepared) => {
                let rate = prepared.rate();
                let snapshot = prepared.snapshot().cloned();
                self.terminal_rate = Some(rate);
                self.terminal_snapshot.clone_from(&snapshot);
                (rate, snapshot, prepared.activation())
            }
            None => (self.terminal_rate?, self.terminal_snapshot.clone(), None),
        };
        if let Some(prepared) = original
            && Self::prepared_input_frames(&prepared).ok()? <= frames
        {
            let selected = Self::prepared_input_frames(&prepared).ok()?;
            self.prepared_quantum = Some(prepared);
            return Some(FrameCount::new(selected));
        }

        let prefix = activation
            .map(PreparedActivation::prefix_frames)
            .transpose()
            .ok()?
            .filter(|prefix| *prefix < frames)
            .unwrap_or(0);
        let cold_start = activation.is_some() && prefix == 0;
        let plan_meta = Self::meta_at_frame(
            meta,
            meta.frame_offset.checked_add(u64::try_from(prefix).ok()?)?,
        );
        let mut prepared = self
            .scheduler_plan_at(plan_meta, frames - prefix, rate)
            .ok()?;
        if prefix > 0 {
            prepared.bind_activation(activation?);
        }
        prepared.bind(snapshot);
        if cold_start {
            self.clear_pending_source();
        }
        let selected = Self::prepared_input_frames(&prepared).ok()?;
        self.prepared_quantum = Some(prepared);
        Some(FrameCount::new(selected))
    }
}
