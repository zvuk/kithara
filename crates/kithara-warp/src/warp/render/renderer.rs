use std::num::{NonZeroU32, NonZeroUsize};

use firewheel_core::param::smoother::{SmoothedParam, SmootherConfig};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunkInfo, AudioSpec};
use kithara_stretch::{
    ElasticBackendConfig, ElasticEngine, ElasticError, ElasticRequest, StretchKind,
};
use kithara_test_macros as kithara;
use num_traits::cast::AsPrimitive;
use tracing::warn;

use super::renderer_target::PreparedTarget;
use crate::{
    ActiveRegion, RegionPlan, RenderReader, RenderSnapshot, StretchControls, WarpConfig,
    temporal::RateTarget,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct PreparedQuantum {
    pub(super) activation: Option<PreparedActivation>,
    pub(super) rate: RateTarget,
    pub(super) speed: f32,
    pub(super) active_frames: usize,
    pub(super) frames: usize,
}

#[derive(Clone, Copy)]
pub(super) struct PreparedActivation {
    pub(super) warm: ElasticRequest,
    pub(super) history_frames: usize,
}

impl PreparedActivation {
    pub(super) fn prefix_frames(self) -> Result<usize, ElasticError> {
        self.history_frames
            .checked_add(self.warm.source_frames())
            .ok_or(ElasticError::SampleCountOverflow)
    }
}

/// Source-timeline exact-span time-stretch driven by shared live controls.
/// Unity speed without a region plan is a byte-identical passthrough.
#[non_exhaustive]
pub struct WarpRenderer<S> {
    pub(super) controls: Arc<StretchControls>,
    pub(super) spec: AudioSpec,
    pub(super) backends: ElasticBackendConfig,
    /// Maximum source frames admitted to one elastic render operation.
    pub(super) source_block_frames: NonZeroUsize,
    /// Latency-sized pooled output discarded while priming an inactive engine.
    pub(super) activation_scratch: Option<SampleBuffer>,
    /// Renderer-owned applied speed. Shared controls contain only the target.
    pub(super) applied_speed: Option<SmoothedParam>,
    pub(super) committed: Option<RenderSnapshot>,
    /// Consumed input retained until the scheduler shell can resize or recycle
    /// it outside the checked render core.
    pub(super) deferred_scratch: Option<SampleBuffer>,
    pub(super) engine: Option<Box<dyn ElasticEngine>>,
    /// Most recent input meta, carried onto each output chunk.
    pub(super) last_input_meta: Option<AudioChunkInfo>,
    /// Exact source coordinate at which the current output scratch begins.
    pub(super) output_start_meta: Option<AudioChunkInfo>,
    /// Oldest sample in the rolling unity history stored in `pending_source`.
    pub(super) passthrough_history_head: Option<usize>,
    /// Earliest metadata represented by `pending_source`.
    pub(super) pending_meta: Option<AudioChunkInfo>,
    /// Source whose cumulative output is still below one representable frame.
    /// Capacity is reserved from the injected pool before the render loop.
    pub(super) pending_source: Option<SampleBuffer>,
    /// Unity chunk retained while the active backend drains its tail.
    /// Its samples occupy `pending_source` without a copy.
    pub(super) pending_unity_meta: Option<AudioChunkInfo>,
    /// Region plan cached from the controls; `Arc::ptr_eq` detects a live swap.
    pub(super) plan: Option<Arc<RegionPlan>>,
    /// Source span and live speed selected by the scheduler for the next render.
    pub(super) prepared_quantum: Option<PreparedQuantum>,
    /// Region covering the playhead - the lookup cursor. `None` forces a
    /// fresh binary search (first chunk, plan swap, region exit, seek).
    pub(super) region: Option<ActiveRegion>,
    /// Maximum output frames between samples of live temporal controls.
    pub(super) render_quantum_frames: Option<NonZeroUsize>,
    /// Exact decoded-source boundary represented by the latest emitted chunk.
    pub(super) rendered_source_end: Option<(u64, NonZeroU32)>,
    /// Engine displaced by a checked render failure. The scheduler shell
    /// drops it from `prepare`, outside `produce_tick_rt`.
    pub(super) retired_engine: Option<Box<dyn ElasticEngine>>,
    /// Interleaved output scratch prepared by the scheduler shell. A produced
    /// chunk takes this buffer; the consumed input becomes its replacement.
    pub(super) scratch: Option<SampleBuffer>,
    pub(super) pools: PoolRegion<S>,
    pub(super) context: RenderReader,
    /// Engine kind currently prepared by the scheduler shell.
    pub(super) current_kind: StretchKind,
    /// Whether previous input ran through the backend. Drives a clean backend
    /// reset when the renderer returns to unity passthrough.
    pub(super) active: bool,
    /// One scheduler-shell rebuild requested after a checked engine failure.
    /// The intent is consumed even when preparation fails.
    pub(super) rebuild_pending: bool,
    /// Reset requested by seek or a return to unity passthrough. The scheduler
    /// shell performs it outside the checked render core.
    pub(super) reset_pending: bool,
    /// Last pitch factor pushed to the backend; avoids redundant updates.
    pub(super) applied_pitch: f64,
    /// Fractional output frames retained across exact-span requests.
    pub(super) output_remainder: f64,
    /// Warm source consumed while priming but not yet represented by output.
    pub(super) primed_source_debt: u64,
    /// Source frames admitted since the last renderer reset.
    pub(super) source_frames_admitted: u64,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) const MAX_OUTPUT_FRAMES: usize = 163_840;
    pub(super) const OUTPUT_ROUNDING_MARGIN: f64 = 0.5;
    /// Re-apply pitch to the backend only when it moves this much.
    pub(super) const RATIO_EPS: f64 = 1e-4;

    /// Build the slot at the source `spec`, driven by the shared `controls`.
    pub(crate) fn new(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        let controls = Arc::clone(config.stretch());
        let current_kind = controls.backend();
        let plan = controls.region_plan();
        let speed = controls.speed();
        let smooth_frames: f32 = config.rate_smooth_frames().get().as_();
        let sample_rate: f32 = spec.sample_rate.get().as_();
        let target = Self::prepare_target(
            current_kind,
            config.backends(),
            config.source_block_frames(),
            spec,
            &pools,
            PreparedTarget::default(),
        );
        let passthrough_history_head = target.engine.is_some().then_some(0);
        Self {
            context,
            committed: None,
            backends: config.backends(),
            engine: target.engine,
            retired_engine: None,
            current_kind,
            controls,
            pools,
            spec,
            source_block_frames: config.source_block_frames(),
            render_quantum_frames: config.render_quantum_frames(),
            prepared_quantum: None,
            applied_speed: (config.rate_smooth_frames().get() > 1).then(|| {
                SmoothedParam::new(
                    speed,
                    SmootherConfig {
                        smooth_seconds: smooth_frames / sample_rate,
                        ..SmootherConfig::default()
                    },
                    spec.sample_rate,
                )
            }),
            applied_pitch: f64::NAN,
            active: false,
            output_remainder: 0.0,
            pending_source: target.pending_source,
            pending_meta: None,
            passthrough_history_head,
            pending_unity_meta: None,
            rendered_source_end: None,
            source_frames_admitted: 0,
            primed_source_debt: 0,
            reset_pending: false,
            rebuild_pending: false,
            last_input_meta: None,
            output_start_meta: None,
            scratch: target.scratch,
            activation_scratch: target.activation_scratch,
            deferred_scratch: None,
            plan,
            region: None,
        }
    }

    /// Whether the renderer can accept another source chunk without dropping it.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        !self.transition_pending()
            && (self.unity_passthrough(self.controls.speed())
                || (self.engine.is_some()
                    && self.pending_source.is_some()
                    && self.scratch.is_some()))
    }

    /// Push `pitch` to the backend when it moved beyond `RATIO_EPS`.
    pub(super) fn apply_pitch(&mut self, pitch: f64) -> Result<(), ElasticError> {
        if !self.applied_pitch.is_nan() && (pitch - self.applied_pitch).abs() <= Self::RATIO_EPS {
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

    pub(super) fn clear_pending_source(&mut self) {
        if let Some(source) = self.pending_source.as_mut() {
            source.clear();
        }
        self.pending_meta = None;
        self.passthrough_history_head = None;
        self.pending_unity_meta = None;
    }

    pub(super) fn clear_render_state(&mut self) {
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }
        if let Some(scratch) = self.activation_scratch.as_mut() {
            scratch.clear();
        }
        self.clear_pending_source();
        self.last_input_meta = None;
        self.output_start_meta = None;
        self.applied_pitch = f64::NAN;
        self.output_remainder = 0.0;
        self.prepared_quantum = None;
        self.rendered_source_end = None;
        self.source_frames_admitted = 0;
        self.primed_source_debt = 0;
        self.active = false;
        self.region = None;
    }

    pub(super) fn commit_rate_render(
        &mut self,
        snapshot: Option<RenderSnapshot>,
        output_frames: usize,
        request_revision: u64,
        applied_rate: f32,
        target_rate: f32,
    ) {
        if let Err(error) = self.advance_speed(target_rate, output_frames) {
            warn!(%error, "time-stretch speed smoothing failed");
        }
        let Some(snapshot) = snapshot else {
            return;
        };
        let Some((committed, session_frame, source_start, source_end)) =
            self.next_render_snapshot(snapshot, output_frames)
        else {
            return;
        };
        kithara::probe_event!(
            rate_applied,
            request_revision,
            applied_rate_bits = applied_rate.to_bits(),
            session_frame,
            source_start,
            source_end
        );
        self.committed = Some(committed);
    }

    pub(super) fn commit_render(&mut self, snapshot: Option<RenderSnapshot>, output_frames: usize) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let Some((committed, output_start, source_start, _)) =
            self.next_render_snapshot(snapshot, output_frames)
        else {
            return;
        };
        kithara::probe_event!(
            render_committed,
            session_epoch = u64::from(committed.context().session_epoch()),
            transport_revision = committed
                .context()
                .transport_revision()
                .map_or(0, u64::from),
            output_start,
            source_start,
            source_end = committed.frontier().source()
        );
        self.committed = Some(committed);
    }

    pub(super) fn defer_scratch(&mut self, replacement: Option<SampleBuffer>) {
        if let Some(replacement) = replacement {
            debug_assert!(self.deferred_scratch.is_none());
            self.deferred_scratch = Some(replacement);
        }
    }

    pub(super) fn held_source_frames(&self) -> u64 {
        if !self.active {
            return 0;
        }
        let pending = u64::try_from(self.pending_frames(usize::from(self.spec.channels.max(1))))
            .unwrap_or(u64::MAX);
        let backend_admitted = self.source_frames_admitted.saturating_sub(pending);
        let latency = self
            .engine
            .as_ref()
            .map_or(0, |engine| engine.capabilities().latency().source_frames());
        let backend_held = u64::try_from(latency)
            .unwrap_or(u64::MAX)
            .min(backend_admitted);
        pending
            .saturating_add(backend_held)
            .saturating_add(self.primed_source_debt)
    }

    pub(super) fn meta_at_frame(meta: AudioChunkInfo, frame_offset: u64) -> AudioChunkInfo {
        let mut start = meta;
        let delta = frame_offset.saturating_sub(meta.frame_offset);
        start.frame_offset = frame_offset;
        start.timestamp = meta.timestamp.saturating_add(
            meta.spec
                .duration_for(delta)
                .unwrap_or(Duration::from_nanos(u64::MAX)),
        );
        if delta > 0 {
            start.source_byte_offset = None;
            start.source_bytes = 0;
        }
        start
    }

    fn next_render_snapshot(
        &self,
        snapshot: RenderSnapshot,
        output_frames: usize,
    ) -> Option<(RenderSnapshot, i64, u64, u64)> {
        if !self.context.is_current(&snapshot) {
            return None;
        }
        let (source_end, _) = self.rendered_source_end?;
        let source_start = self
            .committed
            .as_ref()
            .filter(|previous| {
                previous.context().session_epoch() == snapshot.context().session_epoch()
            })
            .map_or_else(
                || snapshot.frontier().source(),
                |previous| previous.frontier().source(),
            )
            .max(snapshot.frontier().source());
        let committed = snapshot.advance(self.committed.as_ref(), source_end, output_frames)?;
        let output_frames = i64::try_from(output_frames).ok()?;
        let output_start = i64::from(committed.frontier().output()).checked_sub(output_frames)?;
        Some((committed, output_start, source_start, source_end))
    }

    pub(super) fn pending_frames(&self, channels: usize) -> usize {
        if self.transition_pending() || self.passthrough_history_head.is_some() {
            return 0;
        }
        self.pending_source
            .as_deref()
            .map_or(0, |source| source.len() / channels)
    }

    pub(super) fn record_rendered_source_end(
        &mut self,
        meta: AudioChunkInfo,
        held_source_frames: u64,
    ) {
        let admitted = meta.frame_offset.saturating_add(u64::from(meta.frames));
        self.rendered_source_end = Some((
            admitted.saturating_sub(held_source_frames),
            meta.spec.sample_rate,
        ));
    }

    /// Region covering `frame`, plus whether the playhead just crossed out
    /// of a previously resolved region (a plan boundary or a seek).
    pub(super) fn region_for(&mut self, frame: u64) -> ActiveRegion {
        if let Some(r) = self.region
            && r.contains(frame)
        {
            return r;
        }
        let next = self
            .plan
            .as_ref()
            .map_or(ActiveRegion::UNBOUNDED, |p| p.region_at(frame));
        self.region = Some(next);
        next
    }

    /// Exact decoded-source boundary represented by the latest emitted samples.
    #[must_use]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        self.rendered_source_end
    }

    /// Whether this target has elastic DSP and needs worker staging.
    #[must_use]
    pub const fn requires_staging(&self) -> bool {
        true
    }

    pub(super) fn retire_engine(&mut self) {
        debug_assert!(self.retired_engine.is_none());
        self.retired_engine = self.engine.take();
        self.rebuild_pending = true;
    }

    pub(super) fn snap_speed(&mut self) {
        if let Some(applied) = self.applied_speed.as_mut() {
            applied.set_value(self.controls.speed());
            applied.reset_to_target();
        }
        self.prepared_quantum = None;
    }

    /// Pull the live region plan handle; on a swap drop the region cursor.
    pub(super) fn sync_plan(&mut self) {
        let want = self.controls.region_plan();
        let same = match (&self.plan, &want) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            self.plan = want;
            self.region = None;
            self.prepared_quantum = None;
        }
    }

    /// Whether a live active-to-unity transition still owns queued samples.
    #[must_use]
    pub const fn transition_pending(&self) -> bool {
        self.pending_unity_meta.is_some()
    }

    pub(super) fn unity_passthrough(&self, speed: f32) -> bool {
        self.plan.is_none() && (speed - 1.0).abs() <= f32::EPSILON
    }
}
