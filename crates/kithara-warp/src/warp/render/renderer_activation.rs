use kithara_bufpool::HasPool;
use kithara_platform::sync::Arc;
use kithara_signal::{AudioChunk, AudioChunkInfo, FrameCount};
use kithara_stretch::{ElasticError, ElasticRequest};
use kithara_test_macros as kithara;
use num_traits::{ToPrimitive, cast::AsPrimitive};
use tracing::warn;

use super::{
    ScheduledActivationProgress,
    renderer::{PreparedActivation, PreparedDisposition, PreparedQuantum, WarpRenderer},
};
use crate::{RenderSnapshot, SyncMode, WarpCursor};

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    /// Report whether producer output has reached the installed discontinuity.
    pub fn scheduled_activation_progress(&mut self) -> ScheduledActivationProgress {
        self.sync_plan(self.rendered_source_end.map_or(0, |(frame, _)| frame));
        let Some(activation) = self.plan.as_ref().and_then(|plan| plan.activation()) else {
            return ScheduledActivationProgress::AwaitingActivation;
        };
        if let Some(producer_output) = self
            .committed
            .as_ref()
            .map(|snapshot| snapshot.frontier().output())
            && producer_output >= activation.output()
        {
            kithara::probe_event!(
                scheduled_seek_activation_ready,
                producer_output = i64::from(producer_output),
                activation_output = i64::from(activation.output())
            );
            ScheduledActivationProgress::Ready
        } else {
            ScheduledActivationProgress::ProducingOldPcm
        }
    }

    pub(super) fn activate_prepared_quantum(
        &mut self,
        chunk: &mut AudioChunk,
        prepared: PreparedQuantum,
    ) -> Result<(), ElasticError> {
        let Some(activation) = prepared.activation else {
            if self.passthrough_history_head.is_some() {
                self.clear_pending_source();
            }
            return Ok(());
        };
        let prefix_frames = activation.prefix_frames()?;
        let (cue, sample_rate) =
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
        let warm_samples = activation
            .warm
            .source_frames()
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let prefix_samples = prefix_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let active_samples = prepared
            .active_frames
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let active_end = prefix_samples
            .checked_add(active_samples)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let discard_samples = activation
            .warm
            .output_frames()
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let pitch = if self.controls.keylock() {
            1.0
        } else {
            f64::from(prepared.rate.speed())
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
        chunk.samples.copy_within(prefix_samples..active_end, 0);
        chunk.samples.truncate(active_samples);
        let original = chunk.meta;
        chunk.meta = Self::meta_at_frame(
            original,
            original
                .frame_offset
                .checked_add(
                    u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?,
                )
                .ok_or(ElasticError::SampleCountOverflow)?,
        );
        chunk.meta.frames =
            u32::try_from(prepared.active_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        chunk.meta.end_timestamp = original.end_timestamp;
        self.output_start_meta = Some(original);
        self.source_frames_admitted =
            u64::try_from(prefix_frames).map_err(|_| ElasticError::SampleCountOverflow)?;
        self.primed_source_debt = u64::try_from(activation.warm.source_frames())
            .map_err(|_| ElasticError::SampleCountOverflow)?;
        self.active = true;
        Ok(())
    }

    fn activation_latency_frames(&self) -> Option<(usize, usize)> {
        if self.active || self.scratch.is_none() || self.rendered_source_end.is_none() {
            return None;
        }
        let latency = self.engine.as_ref()?.capabilities().latency();
        let history_frames = latency.source_frames();
        let output_frames = latency.output_frames();
        let channels = usize::from(self.spec.channels.max(1));
        let history_samples = history_frames.checked_mul(channels)?;
        if history_frames == 0
            || output_frames == 0
            || self.passthrough_history_head.is_none()
            || self.pending_source.as_deref()?.len() != history_samples
        {
            return None;
        }
        Some((history_frames, output_frames))
    }
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) fn select_context(&mut self, frame: u64) -> Option<RenderSnapshot> {
        self.select_state(frame).and_then(|state| state.snapshot)
    }

    pub(super) fn select_state(&mut self, frame: u64) -> Option<crate::RenderState> {
        let mut state = self.context.load_state()?;
        if let Some(free) = self.plan.as_ref().and_then(|plan| plan.free_handoff()) {
            let handoff = (free.cursor(), free.rate());
            if self.free_handoff_latch.is_none() && frame == free.cursor().source() {
                self.free_handoff_latch = Some(handoff);
            }
            if self.free_handoff_latch == Some(handoff) {
                if state.context.mode() == SyncMode::Off && state.context.rate() == free.rate() {
                    self.free_handoff_latch = None;
                } else {
                    state.context = state.context.clone().with_rate(SyncMode::Off, free.rate());
                    state.snapshot = state
                        .snapshot
                        .map(|snapshot| snapshot.with_context(state.context.clone()));
                }
            }
        }
        if !self.awaiting_activation_before(frame) {
            let region = self.region_for(frame);
            self.rate = state
                .context
                .rate()
                .with_speed(state.context.rate_for(region).as_());
            self.rate_context = Some(state.context.clone());
        }
        Some(state)
    }

    pub(super) fn map_at_exact_frontier(
        &self,
        snapshot: Option<&RenderSnapshot>,
        source: u64,
    ) -> Option<WarpCursor> {
        let activation = self.plan.as_ref()?.activation()?;
        if self.applied_warp_map == Some(activation.revision()) || source != activation.source() {
            return None;
        }
        let snapshot = snapshot?;
        let committed = self.committed.as_ref().filter(|committed| {
            committed.context().session_epoch() == snapshot.context().session_epoch()
        });
        let output = committed.map_or_else(
            || snapshot.frontier().output(),
            |value| value.frontier().output(),
        );
        let selected = output == activation.output();
        selected.then_some(*activation)
    }

    pub(super) fn prepare_discontinuity_context(
        &self,
        snapshot: Option<RenderSnapshot>,
        state: Option<crate::RenderState>,
        source: u64,
    ) -> Option<RenderSnapshot> {
        let Some(activation) = self.plan.as_ref().and_then(|plan| plan.activation()) else {
            return snapshot;
        };
        if self.discontinuity_pending && source == activation.source() {
            return snapshot.map_or_else(
                || {
                    state.map(|state| {
                        RenderSnapshot::preparation_at(
                            state.context,
                            source,
                            activation.output(),
                            activation.revision(),
                        )
                    })
                },
                |snapshot| {
                    Some(snapshot.prepare_at(source, activation.output(), activation.revision()))
                },
            );
        }
        snapshot
    }

    /// Pull the live region plan handle; on a swap drop the region cursor.
    pub(super) fn sync_plan(&mut self, current_source: u64) {
        let want = self.plan_slot.load();
        let same = match (&self.plan, &want) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            let observed_revision = want
                .as_ref()
                .and_then(|plan| plan.activation())
                .map_or(0, |activation| u64::from(activation.revision()));
            kithara::probe_event!(
                region_plan_reader_refreshed,
                observed_revision,
                current_source
            );
            self.plan = want;
            self.region = None;
            self.prepared_context = None;
            self.free_handoff_latch = None;
        }
    }

    /// Whether a live active-to-unity transition still owns queued samples.
    #[must_use]
    pub const fn transition_pending(&self) -> bool {
        self.pending_unity_meta.is_some()
    }

    pub(super) fn unity_passthrough(&self, speed: f32) -> bool {
        (self.plan.as_ref().is_none_or(|plan| {
            plan.segments().is_empty()
                || plan
                    .activation()
                    .is_some_and(|activation| self.applied_warp_map != Some(activation.revision()))
        }) || self
            .rate_context
            .as_ref()
            .is_none_or(|context| context.mode() == SyncMode::Off))
            && (speed - 1.0).abs() <= f32::EPSILON
    }

    fn awaiting_activation_before(&self, source: u64) -> bool {
        self.plan
            .as_ref()
            .and_then(|plan| plan.activation())
            .is_some_and(|activation| {
                self.applied_warp_map != Some(activation.revision())
                    && source != activation.source()
            })
    }

    pub(super) fn cap_before_activation(&self, source: u64, frames: usize) -> usize {
        let Some(activation) = self.plan.as_ref().and_then(|plan| plan.activation()) else {
            return frames;
        };
        if self.applied_warp_map == Some(activation.revision()) {
            return frames;
        }
        let Some(distance) = activation.source().checked_sub(source) else {
            return frames;
        };
        let Ok(distance) = usize::try_from(distance) else {
            return frames;
        };
        if distance == 0 || distance >= frames {
            frames
        } else {
            distance
        }
    }

    /// Select the next source span that fits the configured output quantum.
    pub fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
    ) -> Option<FrameCount> {
        self.sync_plan(meta.frame_offset);
        if self
            .rendered_source_end
            .is_some_and(|(frame, sample_rate)| {
                frame != meta.frame_offset || sample_rate != meta.spec.sample_rate
            })
        {
            self.clear_pending_source();
        }
        let state = self.select_state(meta.frame_offset);
        self.prepared_context = state.as_ref().and_then(|state| state.snapshot.clone());
        let presented = self.prepared_context.take();
        if let Some(prepared) =
            self.prepare_discontinuity_context(presented, state, meta.frame_offset)
        {
            self.prepared_context = Some(prepared);
        }
        let warp_map =
            self.map_at_exact_frontier(self.prepared_context.as_ref(), meta.frame_offset);
        if let Some(activation) = warp_map {
            self.prepared_context = self.prepared_context.take().map(|snapshot| {
                snapshot.prepare_at(
                    meta.frame_offset,
                    activation.output(),
                    activation.revision(),
                )
            });
        }
        let output_rounding_remainder = warp_map.and_then(|activation| {
            self.prepared_context.as_ref().and_then(|snapshot| {
                snapshot
                    .context()
                    .output_rounding_remainder_at(activation.beat())
            })
        });
        let rate = self.rate;
        let speed = rate.speed();
        let result = self
            .prepared_activation(speed)
            .map(|activation| (speed, activation))
            .and_then(|(speed, activation)| {
                let prefix = activation.map_or(Ok(0), PreparedActivation::prefix_frames)?;
                let frame_offset = meta
                    .frame_offset
                    .checked_add(
                        u64::try_from(prefix).map_err(|_| ElasticError::SampleCountOverflow)?,
                    )
                    .ok_or(ElasticError::SampleCountOverflow)?;
                let active_frames = self.source_frames_for_quantum(
                    Self::meta_at_frame(meta, frame_offset),
                    remaining,
                    speed,
                )?;
                let frames = prefix
                    .checked_add(active_frames)
                    .ok_or(ElasticError::SampleCountOverflow)?;
                Ok(PreparedQuantum {
                    disposition: if warp_map.is_some() && activation.is_none() && self.active {
                        PreparedDisposition::CarrierActivation
                    } else {
                        PreparedDisposition::Normal
                    },
                    activation,
                    warp_map: warp_map.map(|activation| activation.revision()),
                    output_rounding_remainder,
                    rate,
                    speed,
                    active_frames,
                    frames,
                })
            });
        match result {
            Ok(prepared) => {
                self.prepared_quantum = Some(prepared);
                Some(FrameCount::new(prepared.frames))
            }
            Err(error) => {
                self.prepared_quantum = None;
                warn!(%error, "time-stretch source quantum sizing failed");
                None
            }
        }
    }

    /// Shrink a prepared source span at true EOF without sampling controls again.
    pub fn prepare_terminal_quantum(
        &mut self,
        _meta: AudioChunkInfo,
        frames: usize,
    ) -> Option<FrameCount> {
        let mut prepared = self.prepared_quantum.take()?;
        if frames == 0 || frames > prepared.frames {
            return None;
        }
        prepared.frames = frames;
        if let Some(activation) = prepared.activation {
            let prefix = activation.prefix_frames().ok()?;
            if frames > prefix {
                prepared.active_frames = frames - prefix;
            } else {
                prepared.active_frames = frames;
                prepared.activation = None;
            }
        } else {
            prepared.active_frames = frames;
        }
        self.prepared_quantum = Some(prepared);
        Some(FrameCount::new(frames))
    }

    pub(super) fn prepared_activation(
        &self,
        speed: f32,
    ) -> Result<Option<PreparedActivation>, ElasticError> {
        if self.unity_passthrough(speed) {
            return Ok(None);
        }
        let Some((history_frames, output_frames)) = self.activation_latency_frames() else {
            return Ok(None);
        };
        let source_frames = output_frames
            .to_f64()
            .map(|frames| (frames * f64::from(speed)).round())
            .and_then(|frames| frames.to_usize())
            .ok_or(ElasticError::SampleCountOverflow)?;
        Ok(Some(PreparedActivation {
            history_frames,
            warm: ElasticRequest::new(source_frames, output_frames)?,
        }))
    }

    pub(super) fn reset_passthrough_history(&mut self) -> Result<(), ElasticError> {
        let channels = usize::from(self.spec.channels.max(1));
        let history_samples = self
            .engine
            .as_ref()
            .ok_or(ElasticError::EnginePreparation("engine is unavailable"))?
            .capabilities()
            .latency()
            .source_frames()
            .checked_mul(channels)
            .ok_or(ElasticError::SampleCountOverflow)?;
        let history = self
            .pending_source
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?;
        history
            .ensure_len(history_samples)
            .map_err(|_| ElasticError::PoolCapacity)?;
        history.fill(0.0);
        self.passthrough_history_head = Some(0);
        Ok(())
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
        let continuous = self.rendered_source_end.is_none_or(|(frame, sample_rate)| {
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

    fn write_passthrough_history(history: &mut [f32], head: usize, source: &[f32]) -> usize {
        debug_assert!(!history.is_empty());
        debug_assert!(source.len() < history.len());
        let first = source.len().min(history.len() - head);
        history[head..head + first].copy_from_slice(&source[..first]);
        let rest = source.len() - first;
        history[..rest].copy_from_slice(&source[first..]);
        (head + source.len()) % history.len()
    }
}
