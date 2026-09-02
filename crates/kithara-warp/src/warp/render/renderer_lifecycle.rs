use kithara_bufpool::{HasPool, SampleBuffer};
use kithara_signal::{AudioChunk, AudioSpec, FrameCount, SampleCount};
use kithara_stretch::ElasticError;
use kithara_test_macros as kithara;
use tracing::warn;

use super::renderer::{PreparedQuantum, WarpRenderer};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    #[kithara::probe(
        request_revision,
        applied_rate_bits,
        session_epoch = u64::from(committed.context().session_epoch()),
        session_frame,
        source_start,
        source_end
    )]
    fn rate_applied(
        &mut self,
        committed: crate::RenderSnapshot,
        request_revision: u64,
        applied_rate_bits: u32,
        session_frame: i64,
        source_start: u64,
        source_end: u64,
    ) {
        self.committed = Some(committed);
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Assemble an output chunk from `scratch` over the source interval that
    /// became presentable since the previous emission. `replacement` is
    /// retained for shell-side preparation before the next checked tick.
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
        let previous_source_end = self.rendered_source_end;
        let output_start = self.output_start_meta.take();
        let admitted = meta.frame_offset.saturating_add(u64::from(meta.frames));
        let source_end = admitted.saturating_sub(held_source_frames);
        let sample_rate = meta.spec.sample_rate;
        let held_duration = match meta.spec.duration_for(admitted).and_then(|admitted| {
            meta.spec
                .duration_for(source_end)
                .map(|source_end| admitted.saturating_sub(source_end))
        }) {
            Ok(duration) => duration,
            Err(error) => {
                warn!(
                    ?error,
                    held_source_frames, "discarding malformed Warp source interval"
                );
                self.scratch.take();
                self.defer_scratch(replacement);
                return None;
            }
        };
        let source_end_timestamp = meta.end_timestamp.saturating_sub(held_duration);
        let (source_start, source_start_timestamp) = match previous_source_end {
            Some((previous, previous_rate, previous_timestamp)) if previous_rate == sample_rate => {
                if source_end < previous {
                    warn!(
                        previous,
                        source_end, "discarding regressive Warp source interval"
                    );
                    self.scratch.take();
                    self.defer_scratch(replacement);
                    return None;
                }
                output_start.map_or((previous, previous_timestamp), |start| {
                    if start.frame_offset == previous {
                        (start.frame_offset, start.timestamp)
                    } else {
                        (previous, previous_timestamp)
                    }
                })
            }
            _ => output_start.map_or((meta.frame_offset, meta.timestamp), |start| {
                (start.frame_offset, start.timestamp)
            }),
        };
        self.record_rendered_source_end(meta, held_source_frames, source_end_timestamp);
        // A non-empty output always carries the live source spec. The default
        // metadata sentinel has zero channels and cannot reach the resampler.
        meta.spec = self.spec;
        meta.frames = u32::try_from(frames.get()).unwrap_or(u32::MAX);
        if source_start != meta.frame_offset {
            meta.source_byte_offset = None;
            meta.source_bytes = 0;
        }
        meta.frame_offset = source_start;
        meta.timestamp = source_start_timestamp;
        meta.end_timestamp = source_end_timestamp;
        let samples = self.scratch.take()?;
        self.defer_scratch(replacement);
        Some(AudioChunk::new(meta, samples))
    }

    fn drain_tail(&mut self, channels: usize) -> Result<bool, ElasticError> {
        if !self.active {
            return Ok(true);
        }
        let frame_limit = self.render_quantum_frames.get();
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
        if start > sample_limit {
            return Err(ElasticError::OutputFrameLimit {
                frames: start / channels,
                limit: frame_limit,
            });
        }
        if start == sample_limit {
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

    #[kithara::measure]
    fn process_active(
        &mut self,
        chunk: AudioChunk,
        prepared: PreparedQuantum,
        direct: bool,
    ) -> Option<AudioChunk> {
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
        if frames > self.source_frame_limit {
            let error = ElasticError::SourceFrameLimit {
                frames,
                limit: self.source_frame_limit,
            };
            warn!(%error, "time-stretch rendering failed; dropping chunk");
            self.defer_scratch(Some(samples));
            return None;
        }
        let rendered = match prepared {
            PreparedQuantum::Exact(exact) => {
                self.render_prepared_exact(meta, &samples, channels, exact, direct)
            }
            PreparedQuantum::FrameCount { rate, speed, .. } => {
                self.exact_cursor = None;
                self.render_active(meta, &samples, speed, channels, frames)
                    .and_then(|()| {
                        let output_frames =
                            self.scratch.as_deref().map_or(0, <[f32]>::len) / channels;
                        Self::advance_speed(self.applied_speed, rate.speed(), output_frames)
                            .map(|next_speed| (next_speed, None))
                    })
            }
        };
        let (next_speed, next_cursor) = match rendered {
            Ok(rendered) => rendered,
            Err(error) => {
                warn!(%error, "time-stretch rendering failed; dropping chunk");
                self.retire_engine();
                self.clear_render_state();
                self.defer_scratch(Some(samples));
                return None;
            }
        };
        self.applied_speed = next_speed;
        self.exact_cursor = next_cursor;
        self.source_frames_admitted = self
            .source_frames_admitted
            .saturating_add(u64::try_from(frames).unwrap_or(u64::MAX));
        let held_source_frames = self.held_source_frames();
        self.emit(Some(samples), held_source_frames)
    }

    #[doc(hidden)]
    pub fn prepare(&mut self, spec: AudioSpec) {
        self.service_target(spec);
    }

    #[doc(hidden)]
    pub fn flush(&mut self) -> Option<AudioChunk> {
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        } else {
            warn!("time-stretch output scratch was not serviced before flush");
            return None;
        }
        self.output_start_meta = None;
        let channels = usize::from(self.spec.channels.max(1));
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
        if complete {
            self.primed_source_debt = 0;
            self.terminal_rate = None;
            self.terminal_snapshot = None;
        }
        output
    }

    fn render_prepared(
        &mut self,
        mut chunk: AudioChunk,
        prepared: PreparedQuantum,
        direct: bool,
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
        chunk.meta.render_revision = prepared.rate().revision();
        if let PreparedQuantum::FrameCount { rate, speed, .. } = &prepared
            && self.can_passthrough(*speed)
        {
            let next_speed =
                match Self::advance_speed(self.applied_speed, rate.speed(), chunk.frames()) {
                    Ok(next_speed) => next_speed,
                    Err(error) => {
                        warn!(%error, "time-stretch speed smoothing failed");
                        self.defer_scratch(Some(chunk.samples));
                        return None;
                    }
                };
            if let Err(error) = self.retain_passthrough_history(chunk.meta, &chunk.samples) {
                warn!(%error, "time-stretch passthrough history retention failed");
                self.retire_engine();
                self.clear_render_state();
            }
            self.record_rendered_source_end(chunk.meta, 0, chunk.meta.end_timestamp);
            self.applied_speed = next_speed;
            self.exact_cursor = None;
            return Some(chunk);
        }
        self.process_active(chunk, prepared, direct)
    }

    #[doc(hidden)]
    pub fn render(&mut self, chunk: AudioChunk) -> Option<AudioChunk> {
        self.prepared_quantum = None;
        let prepared = match self.direct_plan(chunk.meta, chunk.frames()) {
            Ok(prepared) => prepared,
            Err(error) => {
                warn!(%error, "time-stretch render planning failed");
                self.defer_scratch(Some(chunk.samples));
                return None;
            }
        };
        self.render_prepared(chunk, prepared, true)
    }

    fn quantum_source_shape_matches(prepared: &PreparedQuantum, actual: usize) -> bool {
        let expected = match Self::prepared_input_frames(prepared) {
            Ok(expected) => expected,
            Err(error) => {
                warn!(%error, "time-stretch prepared source sizing failed");
                return false;
            }
        };
        if actual == expected {
            return true;
        }
        warn!(
            actual,
            expected, "time-stretch quantum source shape changed"
        );
        false
    }

    /// Render the source quantum paired with the speed sampled by
    /// [`Self::prepare_quantum`].
    #[doc(hidden)]
    #[kithara::measure]
    pub fn render_quantum(&mut self, mut chunk: AudioChunk) -> Option<AudioChunk> {
        let Some(prepared) = self.prepared_quantum.take() else {
            warn!("time-stretch quantum was not prepared before rendering");
            self.defer_scratch(Some(chunk.samples));
            return None;
        };
        let snapshot = prepared.snapshot().cloned();
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| !self.context.is_current(snapshot))
        {
            warn!("time-stretch quantum belongs to an inactive session epoch");
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        if !Self::quantum_source_shape_matches(&prepared, chunk.frames()) {
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        if let Err(error) = self.activate_prepared_quantum(&mut chunk, &prepared) {
            warn!(%error, "time-stretch activation failed; dropping chunk");
            self.retire_engine();
            self.clear_render_state();
            self.defer_scratch(Some(chunk.samples));
            return None;
        }
        let rate = prepared.rate();
        let applied_speed = prepared.speed();
        let output = self.render_prepared(chunk, prepared, false);
        if let (Some(snapshot), Some(output)) = (snapshot, output.as_ref()) {
            if let Some((committed, session_frame, source_end)) =
                self.next_render_snapshot(snapshot, output.frames())
            {
                self.rate_applied(
                    committed,
                    rate.revision(),
                    applied_speed.to_bits(),
                    i64::from(session_frame),
                    output.meta.frame_offset,
                    source_end,
                );
            } else {
                warn!("time-stretch quantum produced an invalid presentation frontier");
            }
        }
        output
    }

    #[doc(hidden)]
    pub fn reset(&mut self) {
        self.reset_pending = true;
        self.clear_render_state();
        self.committed = None;
        self.snap_speed();
    }
}
