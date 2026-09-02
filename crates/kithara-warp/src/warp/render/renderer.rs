use std::num::{NonZeroU32, NonZeroUsize};

use firewheel_core::param::smoother::{SmoothedParam, SmootherConfig};
use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunkInfo, AudioSpec};
use kithara_stretch::{
    ElasticCursor, ElasticEngine, ElasticError, ElasticRequest, ElasticSpanPlan, StretchKind,
};
use num_traits::cast::AsPrimitive;

use super::renderer_target::PreparedTarget;
use crate::{
    ActiveRegion, RegionPlan, RenderReader, RenderSnapshot, StretchControls, WarpConfig,
    temporal::RateTarget,
};

#[cfg(test)]
mod tests;

pub(super) struct PreparedExact {
    pub(super) activation: Option<PreparedActivation>,
    pub(super) next_speed: SmoothedParam,
    pub(super) plan: ElasticSpanPlan,
    pub(super) rate: RateTarget,
    pub(super) snapshot: Option<RenderSnapshot>,
    pub(super) speed: f32,
}

#[derive(Clone, Copy)]
pub(super) struct PreparedActivation {
    pub(super) history_frames: usize,
    pub(super) warm: ElasticRequest,
}

impl PreparedActivation {
    pub(super) fn prefix_frames(self) -> Result<usize, ElasticError> {
        self.history_frames
            .checked_add(self.warm.source_frames())
            .ok_or(ElasticError::SampleCountOverflow)
    }
}

pub(super) enum PreparedQuantum {
    Exact(PreparedExact),
    FrameCount {
        activation: Option<PreparedActivation>,
        source_frames: usize,
        rate: RateTarget,
        snapshot: Option<RenderSnapshot>,
        speed: f32,
    },
}

impl PreparedQuantum {
    pub(super) fn bind(&mut self, snapshot: Option<RenderSnapshot>) {
        match self {
            Self::Exact(exact) => exact.snapshot = snapshot,
            Self::FrameCount {
                snapshot: bound, ..
            } => *bound = snapshot,
        }
    }

    pub(super) fn snapshot(&self) -> Option<&RenderSnapshot> {
        match self {
            Self::Exact(exact) => exact.snapshot.as_ref(),
            Self::FrameCount { snapshot, .. } => snapshot.as_ref(),
        }
    }

    pub(super) const fn rate(&self) -> RateTarget {
        match self {
            Self::Exact(exact) => exact.rate,
            Self::FrameCount { rate, .. } => *rate,
        }
    }

    pub(super) const fn activation(&self) -> Option<PreparedActivation> {
        match self {
            Self::Exact(exact) => exact.activation,
            Self::FrameCount { activation, .. } => *activation,
        }
    }

    pub(super) fn bind_activation(&mut self, activation: PreparedActivation) {
        match self {
            Self::Exact(exact) => exact.activation = Some(activation),
            Self::FrameCount {
                activation: bound, ..
            } => *bound = Some(activation),
        }
    }

    pub(super) const fn speed(&self) -> f32 {
        match self {
            Self::Exact(exact) => exact.speed,
            Self::FrameCount { speed, .. } => *speed,
        }
    }
}

/// Source-timeline exact-span time-stretch driven by shared live controls.
/// Unity speed without a region plan is a byte-identical passthrough.
#[non_exhaustive]
pub struct WarpRenderer<S> {
    pub(super) context: RenderReader,
    pub(super) committed: Option<RenderSnapshot>,
    pub(super) controls: Arc<StretchControls>,
    pub(super) engine: Option<Box<dyn ElasticEngine>>,
    /// Engine displaced by a checked render failure. The scheduler shell
    /// drops it from `prepare`, outside `produce_tick_rt`.
    pub(super) retired_engine: Option<Box<dyn ElasticEngine>>,
    /// Most recent input meta, carried onto each output chunk.
    pub(super) last_input_meta: Option<AudioChunkInfo>,
    /// Exact source coordinate at which the current output scratch begins.
    pub(super) output_start_meta: Option<AudioChunkInfo>,
    /// Region plan cached from the controls; `Arc::ptr_eq` detects a live swap.
    pub(super) plan: Option<Arc<RegionPlan>>,
    /// Region covering the playhead - the lookup cursor. `None` forces a
    /// fresh binary search (first chunk, plan swap, region exit, seek).
    pub(super) region: Option<ActiveRegion>,
    /// Exact rate plan paired with the source quantum prepared by the scheduler.
    pub(super) prepared_quantum: Option<PreparedQuantum>,
    /// Rate sampled before true EOF, retained while a partial terminal input is partitioned.
    pub(super) terminal_rate: Option<RateTarget>,
    /// Render context sampled with `terminal_rate`.
    pub(super) terminal_snapshot: Option<RenderSnapshot>,
    /// Renderer-owned applied speed. Shared controls contain only the target.
    pub(super) applied_speed: SmoothedParam,
    /// Exact source coordinate committed with the applied speed after rendering.
    pub(super) exact_cursor: Option<ElasticCursor>,
    pub(super) pools: PoolRegion<S>,
    pub(super) spec: AudioSpec,
    /// Maximum output frames between samples of live temporal controls.
    pub(super) render_quantum_frames: NonZeroUsize,
    /// Maximum source span accepted by the prepared elastic engine.
    pub(super) source_frame_limit: usize,
    /// Maximum interleaved output span retained by renderer scratch.
    pub(super) scratch_frame_limit: usize,
    /// Engine kind currently prepared by the scheduler shell.
    pub(super) current_kind: StretchKind,
    /// Latency-sized pooled output discarded while priming an inactive engine.
    pub(super) activation_scratch: Option<SampleBuffer>,
    /// Interleaved output scratch prepared by the scheduler shell. A produced
    /// chunk takes this buffer; the consumed input becomes its replacement.
    pub(super) scratch: Option<SampleBuffer>,
    /// Consumed input retained until the scheduler shell can resize or recycle
    /// it outside the checked render core.
    pub(super) deferred_scratch: Option<SampleBuffer>,
    /// Whether previous input ran through the backend. Once active, the
    /// resident engine also owns exact-unity rendering.
    pub(super) active: bool,
    /// Last pitch factor pushed to the backend; avoids redundant updates.
    pub(super) applied_pitch: f64,
    /// Fractional output frames retained across exact-span requests.
    pub(super) output_remainder: f64,
    /// Source whose cumulative output is still below one representable frame.
    /// Capacity is reserved from the injected pool before the render loop.
    pub(super) pending_source: Option<SampleBuffer>,
    /// Earliest metadata represented by `pending_source`.
    pub(super) pending_meta: Option<AudioChunkInfo>,
    /// Oldest sample in the rolling passthrough history stored in `pending_source`.
    pub(super) passthrough_history_head: Option<usize>,
    /// Exact decoded-source boundary represented by the latest emitted chunk.
    pub(super) rendered_source_end: Option<(u64, NonZeroU32, Duration)>,
    /// Source frames admitted since the last renderer reset.
    pub(super) source_frames_admitted: u64,
    /// Warm source consumed while priming but not yet represented by output.
    pub(super) primed_source_debt: u64,
    /// Reset requested by a timeline discontinuity. The scheduler shell
    /// performs it outside the checked render core.
    pub(super) reset_pending: bool,
    /// One scheduler-shell rebuild requested after a checked engine failure.
    /// The intent is consumed even when preparation fails.
    pub(super) rebuild_pending: bool,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) const DIRECT_OUTPUT_FRAME_LIMIT: usize = 163_840;
    pub(super) const RESIDENT_SOURCE_FRAME_LIMIT: usize = 8192;
    const DIRECT_SOURCE_FRAME_LIMIT: usize = Self::DIRECT_OUTPUT_FRAME_LIMIT * 4;
    /// Avoids FFI updates for sub-audible floating-point noise.
    pub(super) const PITCH_UPDATE_EPSILON: f64 = 1e-4;

    /// Build the slot at the source `spec`, driven by the shared `controls`.
    pub(crate) fn new(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        Self::new_with_limits(
            config,
            context,
            spec,
            pools,
            Self::DIRECT_SOURCE_FRAME_LIMIT,
            Self::DIRECT_OUTPUT_FRAME_LIMIT,
        )
    }

    pub(crate) fn new_quantum(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        Self::new_with_limits(
            config,
            context,
            spec,
            pools,
            Self::RESIDENT_SOURCE_FRAME_LIMIT,
            config.render_quantum_frames().get(),
        )
    }

    fn new_with_limits(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
        source_frame_limit: usize,
        scratch_frame_limit: usize,
    ) -> Self {
        let controls = Arc::clone(config.stretch());
        let current_kind = controls.backend();
        let plan = controls.region_plan();
        let speed = controls.speed();
        let smooth_frames: f32 = config.rate_smooth_frames().get().as_();
        let sample_rate: f32 = spec.sample_rate.get().as_();
        let target = Self::prepare_target(
            current_kind,
            spec,
            &pools,
            source_frame_limit,
            scratch_frame_limit,
            PreparedTarget::default(),
        );
        Self {
            context,
            committed: None,
            engine: target.engine,
            retired_engine: None,
            current_kind,
            controls,
            pools,
            spec,
            render_quantum_frames: config.render_quantum_frames(),
            source_frame_limit,
            scratch_frame_limit,
            prepared_quantum: None,
            terminal_rate: None,
            terminal_snapshot: None,
            applied_speed: SmoothedParam::new(
                speed,
                SmootherConfig {
                    smooth_seconds: smooth_frames / sample_rate,
                    ..SmootherConfig::default()
                },
                spec.sample_rate,
            ),
            exact_cursor: None,
            applied_pitch: f64::NAN,
            active: false,
            output_remainder: 0.0,
            pending_source: target.pending_source,
            pending_meta: None,
            passthrough_history_head: None,
            rendered_source_end: None,
            source_frames_admitted: 0,
            primed_source_debt: 0,
            reset_pending: false,
            rebuild_pending: false,
            last_input_meta: None,
            output_start_meta: None,
            activation_scratch: target.activation_scratch,
            scratch: target.scratch,
            deferred_scratch: None,
            plan,
            region: None,
        }
    }

    pub(super) fn clear_pending_source(&mut self) {
        if let Some(source) = self.pending_source.as_mut() {
            source.clear();
        }
        self.pending_meta = None;
        self.passthrough_history_head = None;
    }

    pub(super) fn retire_engine(&mut self) {
        debug_assert!(self.retired_engine.is_none());
        self.retired_engine = self.engine.take();
        self.rebuild_pending = true;
    }

    pub(super) fn clear_render_state(&mut self) {
        if let Some(scratch) = self.activation_scratch.as_mut() {
            scratch.clear();
        }
        if let Some(scratch) = self.scratch.as_mut() {
            scratch.clear();
        }
        self.clear_pending_source();
        self.last_input_meta = None;
        self.output_start_meta = None;
        self.applied_pitch = f64::NAN;
        self.output_remainder = 0.0;
        self.prepared_quantum = None;
        self.terminal_rate = None;
        self.terminal_snapshot = None;
        self.exact_cursor = None;
        self.rendered_source_end = None;
        self.source_frames_admitted = 0;
        self.primed_source_debt = 0;
        self.active = false;
        self.region = None;
    }

    pub(super) fn defer_scratch(&mut self, replacement: Option<SampleBuffer>) {
        if let Some(replacement) = replacement {
            debug_assert!(self.deferred_scratch.is_none());
            self.deferred_scratch = Some(replacement);
        }
    }

    pub(super) fn snap_speed(&mut self) {
        self.applied_speed.set_value(self.controls.speed());
        self.applied_speed.reset_to_target();
        self.prepared_quantum = None;
        self.exact_cursor = None;
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
            self.exact_cursor = None;
        }
    }

    pub(super) fn unity_passthrough(&self, speed: f32) -> bool {
        self.plan.is_none() && (speed - 1.0).abs() <= f32::EPSILON
    }

    pub(super) fn can_passthrough(&self, speed: f32) -> bool {
        let channels = usize::from(self.spec.channels.max(1));
        !self.active && self.pending_frames(channels) == 0 && self.unity_passthrough(speed)
    }

    pub(super) fn pending_frames(&self, channels: usize) -> usize {
        if self.passthrough_history_head.is_some() {
            return 0;
        }
        self.pending_source
            .as_deref()
            .map_or(0, |source| source.len() / channels)
    }

    /// Whether the renderer can accept another source chunk without dropping it.
    #[doc(hidden)]
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        self.can_passthrough(self.controls.speed())
            || (self.engine.is_some() && self.pending_source.is_some() && self.scratch.is_some())
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

    pub(super) fn record_rendered_source_end(
        &mut self,
        meta: AudioChunkInfo,
        held_source_frames: u64,
        timestamp: Duration,
    ) {
        let admitted = meta.frame_offset.saturating_add(u64::from(meta.frames));
        self.rendered_source_end = Some((
            admitted.saturating_sub(held_source_frames),
            meta.spec.sample_rate,
            timestamp,
        ));
    }

    pub(super) fn next_render_snapshot(
        &self,
        snapshot: RenderSnapshot,
        output_frames: usize,
    ) -> Option<(RenderSnapshot, crate::SessionFrame, u64)> {
        let (source, _, _) = self.rendered_source_end?;
        let committed = snapshot.advance(self.committed.as_ref(), source, output_frames)?;
        let output_frames = i64::try_from(output_frames).ok()?;
        let output_start = i64::from(committed.frontier().output()).checked_sub(output_frames)?;
        Some((committed, crate::SessionFrame::new(output_start), source))
    }

    /// Last context and frontier committed by a successful worker quantum.
    #[doc(hidden)]
    #[must_use]
    pub fn render_snapshot(&self) -> Option<&RenderSnapshot> {
        self.committed.as_ref()
    }

    /// Exact decoded-source boundary represented by the latest emitted samples.
    #[doc(hidden)]
    #[must_use]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        match self.rendered_source_end {
            Some((frame, sample_rate, _)) => Some((frame, sample_rate)),
            None => None,
        }
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
}
