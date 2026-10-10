use kithara_bufpool::HasPool;
use kithara_signal::{AudioChunk, AudioChunkInfo, FrameCount};
use kithara_stretch::ElasticError;
use kithara_test_macros as kithara;
use num_traits::ToPrimitive;

use super::core::{PreparedQuantum, WarpRenderer};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Prime the engine from the residency so its first output is the source
    /// frame presented last; the engine then reads on from the residency.
    pub(in crate::render) fn activate_prepared_quantum(
        &mut self,
        chunk: &AudioChunk,
        prepared: PreparedQuantum,
    ) -> Result<(), ElasticError> {
        let Some(activation) = prepared.activation else {
            return Ok(());
        };
        let prefix_frames = activation.prefix_frames()?;
        let (cue, sample_rate) = self
            .rendered_source_end
            .unwrap_or((chunk.meta.frame_offset, chunk.meta.spec.sample_rate));
        if chunk.meta.spec.sample_rate != sample_rate {
            return Err(ElasticError::DiscontinuousSource {
                expected: cue.to_f64().ok_or(ElasticError::SampleCountOverflow)?,
                actual: chunk
                    .meta
                    .frame_offset
                    .to_f64()
                    .ok_or(ElasticError::SampleCountOverflow)?,
            });
        }
        if chunk.frames() != prepared.frames {
            return Err(ElasticError::SourceFrameLimit {
                frames: chunk.frames(),
                limit: prepared.frames,
            });
        }

        let channels = usize::from(self.spec.channels.max(1));
        let history_samples = activation
            .history_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let discard_samples = activation
            .warm
            .output_frames()
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let lookahead_end = cue
            .checked_add(
                u64::try_from(activation.history_frames)
                    .map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        let prefix_end = cue
            .checked_add(
                u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?,
            )
            .ok_or(ElasticError::SampleCountOverflow)?;
        let pitch = if self.current_keylock {
            1.0
        } else {
            f64::from(prepared.rate.speed())
        };
        self.apply_pitch(pitch)?;

        let residency = self.residency.as_ref().ok_or(ElasticError::PoolCapacity)?;
        let history_start = i64::try_from(cue)
            .ok()
            .and_then(|cue| cue.checked_sub(i64::try_from(activation.history_frames).ok()?))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let range = residency.range(history_start.max(residency.start), cue, channels)?;
        let resident_history = &residency.samples[range];
        let history = if resident_history.len() == history_samples {
            resident_history
        } else {
            let history = self
                .pending_source
                .as_mut()
                .ok_or(ElasticError::PoolCapacity)?;
            history
                .ensure_len(history_samples)
                .map_err(|_| ElasticError::PoolCapacity)?;
            let missing = history_samples
                .checked_sub(resident_history.len())
                .ok_or(ElasticError::SampleCountOverflow)?;
            history[..missing].fill(0.0);
            history[missing..].copy_from_slice(resident_history);
            history.as_ref()
        };
        let resident_from = |start: u64, end: u64| {
            i64::try_from(start)
                .map_err(|_| ElasticError::SampleCountOverflow)
                .and_then(|start| residency.range(start, end, channels))
        };
        let lookahead = &residency.samples[resident_from(cue, lookahead_end)?];
        let warm = &residency.samples[resident_from(lookahead_end, prefix_end)?];
        let scratch = self
            .activation_scratch
            .as_mut()
            .ok_or(ElasticError::EnginePreparation(
                "activation scratch is unavailable",
            ))?;
        scratch
            .ensure_len(discard_samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        kithara::probe_event!(
            prime_activation,
            request_revision = prepared.rate.revision(),
            target_rate_bits = prepared.rate.speed().to_bits(),
            source_frames = activation.warm.source_frames(),
            output_frames = activation.warm.output_frames()
        );
        self.engine
            .as_mut()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .prime(activation.warm, history, lookahead, warm, scratch)?;
        scratch.clear();

        self.clear_pending_source();
        self.output_start_meta = Some(Self::meta_at_frame(chunk.meta, cue));
        self.resident_feed = Some(prefix_end);
        self.source_frames_admitted =
            u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        self.primed_source_debt = u64::try_from(activation.warm.source_frames())
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        self.active = true;
        Ok(())
    }

    /// Select the next source span that fits the configured output quantum.
    /// A quantum at the renderer's own speed renders at most `output_limit`
    /// output frames.
    ///
    /// # Errors
    /// Returns pending activation or the geometry/engine admission error.
    pub fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<FrameCount, crate::WarpRenderError> {
        if let Some(prepared) = self.prepared_quantum {
            if prepared.source_start != meta.frame_offset {
                return Err(crate::WarpRenderError::OutstandingQuantum);
            }
            if prepared.source_span.is_none_or(|span| {
                usize::try_from(span.output_frames()).is_ok_and(|frames| frames <= output_limit)
            }) {
                return Ok(FrameCount::new(prepared.frames));
            }
            self.prepared_quantum = None;
        }
        if self.transition_pending() || self.engine_outdated() {
            return Err(crate::WarpRenderError::NeedsService);
        }
        if !self.requires_staging() && self.plan.is_some() {
            return Err(crate::WarpRenderError::UnsupportedRegionPlan);
        }
        self.terminal_source_end = None;
        if !self.requires_staging()
            || (self.plan.is_none()
                && (self.trajectory.constant_unity()
                    || (!self.current_keylock && self.trajectory.unity_interval())))
        {
            let prepared = self.prepare_unity_quantum(meta, remaining, output_limit)?;
            let frames = prepared.frames;
            self.prepared_quantum = Some(prepared);
            return Ok(FrameCount::new(frames));
        }
        let prepared = self.prepare_projected_quantum(meta, remaining, output_limit)?;
        self.prepared_quantum = Some(prepared);
        Ok(FrameCount::new(prepared.frames))
    }

    fn prepare_unity_quantum(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        let outputs = self
            .trajectory
            .output_limit(output_limit)
            .min(self.source_block_frames.get())
            .min(
                self.render_quantum_frames
                    .map_or(remaining, |limit| limit.get().min(remaining)),
            );
        if outputs == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let span = self.mapping_span(meta.frame_offset, meta.spec.sample_rate, outputs)?;
        let (numerator, denominator) = span
            .source_ratio_at(span.output_frames() - 1)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let end = u64::try_from(numerator.div_ceil(denominator.get()))
            .ok()
            .and_then(|frame| frame.checked_add(1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frames = usize::try_from(end.saturating_sub(meta.frame_offset))
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        Ok(PreparedQuantum {
            source_span: Some(span),
            activation: None,
            rate: self.rate,
            speed: 1.0,
            source_start: meta.frame_offset,
            active_frames: frames,
            frames,
            landing_frames: Some(outputs),
        })
    }

    fn prepare_projected_quantum(
        &self,
        meta: AudioChunkInfo,
        remaining: usize,
        output_limit: usize,
    ) -> Result<PreparedQuantum, ElasticError> {
        let capabilities = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities();
        let output_limit = self.trajectory.output_limit(output_limit);
        let quantum = self.render_quantum_frames.map_or_else(
            || remaining.min(capabilities.max_output_frames()),
            |limit| limit.get().min(capabilities.max_output_frames()),
        );
        let outputs = output_limit
            .min(quantum)
            .min(self.source_block_frames.get());
        if outputs == 0 {
            return Err(ElasticError::EmptyOutput);
        }
        let span = self.mapping_span(meta.frame_offset, meta.spec.sample_rate, outputs)?;
        let outputs =
            usize::try_from(span.output_frames()).map_err(|_| ElasticError::SampleCountOverflow)?;
        let lookahead = if self.current_keylock {
            self.projection_lookahead()?
        } else {
            0
        };
        let offset = lookahead
            .checked_add(outputs - 1)
            .and_then(|offset| u64::try_from(offset).ok())
            .ok_or(ElasticError::SampleCountOverflow)?;
        let (numerator, denominator) =
            self.mapped_position(meta.frame_offset, meta.spec.sample_rate, offset)?;
        let correction = self.plan.as_ref().map_or(1.0, |plan| {
            plan.segments().iter().fold(1.0_f64, |minimum, segment| {
                minimum.min(segment.ratio_correction())
            })
        });
        let radius = if f64::from(self.trajectory.speed_bounds()?.1) <= correction {
            0
        } else {
            crate::consts::SOURCE_RADIUS
        };
        let end = u64::try_from(numerator.div_ceil(denominator.get()))
            .ok()
            .and_then(|frame| frame.checked_add(radius + 1))
            .ok_or(ElasticError::SampleCountOverflow)?;
        let frames = usize::try_from(end.saturating_sub(meta.frame_offset))
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        Ok(PreparedQuantum {
            source_span: Some(span),
            activation: None,
            rate: self.rate,
            speed: self.trajectory.speed()?,
            source_start: meta.frame_offset,
            active_frames: frames,
            frames,
            landing_frames: Some(outputs),
        })
    }

    /// Shrink a prepared source span at true EOF without sampling controls again.
    pub fn prepare_terminal_quantum(&mut self, frames: usize) -> Option<FrameCount> {
        let mut prepared = self.prepared_quantum.take()?;
        if frames == 0 || frames > prepared.frames {
            return None;
        }
        let end = prepared
            .source_start
            .checked_add(u64::try_from(frames).ok()?)?;
        self.terminal_source_end = Some(end);
        if let Some(span) = prepared.source_span {
            let mut outputs = span.output_frames();
            while outputs > 0 {
                let (numerator, denominator) = span.source_ratio_at(outputs - 1)?;
                if numerator / denominator.get() < u128::from(end) {
                    break;
                }
                outputs -= 1;
            }
            prepared.source_span = Some(span.for_output_range(0..outputs)?);
            prepared.landing_frames = Some(usize::try_from(outputs).ok()?);
        }
        let shrink = prepared.frames - frames;
        if shrink > 0 && prepared.source_span.is_none() {
            prepared.landing_frames = None;
        }
        prepared.frames = frames;
        if prepared.active_frames > shrink {
            prepared.active_frames -= shrink;
        } else {
            prepared.active_frames = frames;
            prepared.activation = None;
        }
        self.prepared_quantum = Some(prepared);
        Some(FrameCount::new(frames))
    }
}
