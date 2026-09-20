use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunkInfo, AudioSpec};
use kithara_stretch::{
    ElasticBackendConfig, ElasticEngine, ElasticError, ElasticRequest, StretchKind,
};
use kithara_test_macros as kithara;

use super::renderer_target::PreparedTarget;
use crate::{
    RenderReader, RenderSnapshot, StretchControls, WarpConfig, WarpMapRevision, WarpPlan,
    WarpPlanSlot, temporal::RateTarget,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct PreparedQuantum {
    pub(super) disposition: PreparedDisposition,
    pub(super) activation: Option<PreparedActivation>,
    pub(super) warp_map: Option<WarpMapRevision>,
    pub(super) output_rounding_remainder: Option<f64>,
    pub(super) rate: RateTarget,
    pub(super) speed: f32,
    pub(super) active_frames: usize,
    pub(super) frames: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum PreparedDisposition {
    Normal,
    CarrierActivation,
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

/// Source-timeline exact-span time-stretch driven by the published output rate.
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
    /// Initial prefill rate, then the latest rate published by the output owner.
    pub(super) rate: RateTarget,
    /// Free context held from its exact map activation until live Off observes it.
    ///
    /// The latch belongs to the loaded immutable plan and its map cursor;
    /// `sync_plan` clears it before another plan can be selected.
    pub(super) free_handoff_latch: Option<(crate::WarpCursor, RateTarget)>,
    pub(super) prepared_context: Option<RenderSnapshot>,
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
    /// Plan cached from `plan_slot`; `Arc::ptr_eq` detects a live swap.
    pub(super) plan: Option<Arc<WarpPlan>>,
    /// Live plan of the rendered item, shared with the deck that installs it.
    pub(super) plan_slot: Arc<WarpPlanSlot>,
    /// Last rate the installed projection named, held for the frames past the
    /// end of the recording, where it names none. Cleared with the plan.
    pub(super) projected_rate_held: Option<f64>,
    /// Source span and live speed selected by the scheduler for the next render.
    pub(super) prepared_quantum: Option<PreparedQuantum>,
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
    /// Pitch mode represented by the currently prepared engine.
    pub(super) current_keylock: bool,
    /// Whether previous input ran through the backend. Drives a clean backend
    /// reset when the renderer returns to unity passthrough.
    pub(super) active: bool,
    /// One scheduler-shell rebuild requested after a checked engine failure.
    /// The intent is consumed even when preparation fails.
    pub(super) rebuild_pending: bool,
    /// Reset requested by seek or a return to unity passthrough. The scheduler
    /// shell performs it outside the checked render core.
    pub(super) reset_pending: bool,
    /// A source discontinuity may prepare its exact future activation without
    /// advancing the callback-owned presentation frontier.
    pub(super) discontinuity_pending: bool,
    /// Last pitch factor pushed to the backend; avoids redundant updates.
    pub(super) applied_pitch: f64,
    /// Fractional output frames retained across exact-span requests.
    pub(super) output_remainder: f64,
    /// Warm source consumed while priming but not yet represented by output.
    pub(super) primed_source_debt: u64,
    /// Source frames admitted since the last renderer reset.
    pub(super) source_frames_admitted: u64,
    /// Latest warp map whose exact source/output anchor was rendered.
    pub(super) applied_warp_map: Option<WarpMapRevision>,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) const MAX_OUTPUT_FRAMES: usize = 163_840;
    pub(super) const OUTPUT_ROUNDING_MARGIN: f64 = 0.5;
    /// Re-apply pitch to the backend only when it moves this much.
    pub(super) const RATIO_EPS: f64 = 1e-4;

    /// Speed of an item whose projection owns the rate but has named none yet.
    ///
    /// The recording is heard as recorded until the projection answers. It is
    /// not the listener's target: that target belongs to an item the
    /// projection does not place, and reading it here would let a projected
    /// item start at a rate the projection never prescribed.
    pub(super) const UNNAMED_SPEED: f32 = 1.0;

    /// Build the slot at the source `spec`, driven by the shared `controls`.
    pub(crate) fn new(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
        plan_slot: Arc<WarpPlanSlot>,
    ) -> Self {
        let controls = Arc::clone(config.stretch());
        let current_kind = controls.backend();
        let current_keylock = controls.keylock();
        let plan = plan_slot.load();
        let rate = controls.rate_target();
        let rate = if plan.as_deref().is_some_and(WarpPlan::follows_output) {
            rate.with_speed(Self::UNNAMED_SPEED)
        } else {
            rate
        };
        let target = Self::prepare_target(
            current_kind,
            current_keylock,
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
            current_keylock,
            controls,
            plan_slot,
            projected_rate_held: None,
            pools,
            spec,
            source_block_frames: config.source_block_frames(),
            render_quantum_frames: config.render_quantum_frames(),
            prepared_quantum: None,
            rate,
            free_handoff_latch: None,
            prepared_context: None,
            applied_pitch: f64::NAN,
            active: false,
            output_remainder: 0.0,
            pending_source: target.pending_source,
            pending_meta: None,
            passthrough_history_head,
            pending_unity_meta: None,
            rendered_source_end: None,
            source_frames_admitted: 0,
            applied_warp_map: None,
            primed_source_debt: 0,
            reset_pending: false,
            discontinuity_pending: false,
            rebuild_pending: false,
            last_input_meta: None,
            output_start_meta: None,
            scratch: target.scratch,
            activation_scratch: target.activation_scratch,
            deferred_scratch: None,
            plan,
        }
    }

    /// Whether the renderer can accept another source chunk without dropping it.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        !self.transition_pending()
            && (self.unity_passthrough(self.rate.speed())
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
        self.prepared_context = None;
        self.free_handoff_latch = None;
        self.rendered_source_end = None;
        self.applied_warp_map = None;
        self.source_frames_admitted = 0;
        self.primed_source_debt = 0;
        self.active = false;
    }
    pub(super) fn commit_rate_render(
        &mut self,
        snapshot: Option<RenderSnapshot>,
        output_frames: usize,
        render_revision: u64,
        applied_rate: f32,
    ) {
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
            request_revision = kithara_signal::render_rate_revision(render_revision),
            applied_rate_bits = applied_rate.to_bits(),
            session_frame,
            source_start,
            source_end
        );
        self.commit(committed, render_revision, session_frame, source_start);
    }

    pub(super) fn bind_output_identity(
        snapshot: Option<RenderSnapshot>,
        render_revision: u64,
    ) -> Option<RenderSnapshot> {
        let warp_map = NonZeroU64::new(kithara_signal::render_warp_map_revision(render_revision))
            .map(WarpMapRevision::from_raw);
        snapshot.map(|snapshot| crate::temporal::rebind_warp_map(snapshot, warp_map))
    }

    pub(super) fn commit_render(
        &mut self,
        snapshot: Option<RenderSnapshot>,
        output_frames: usize,
        render_revision: u64,
    ) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let Some((committed, output_start, source_start, _)) =
            self.next_render_snapshot(snapshot, output_frames)
        else {
            return;
        };
        self.commit(committed, render_revision, output_start, source_start);
    }

    /// The single place a render becomes the committed one, so every committed
    /// render publishes the session axis it fixed regardless of the path that
    /// produced it.
    pub(super) fn commit(
        &mut self,
        committed: RenderSnapshot,
        render_revision: u64,
        output_start: i64,
        source_start: u64,
    ) {
        kithara::probe_event!(
            render_committed,
            render_revision,
            output_start,
            output_end = i64::from(committed.frontier().output()),
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

    /// Exact decoded-source boundary represented by the latest emitted samples.
    #[must_use]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        self.rendered_source_end
    }

    /// Returns the coherent same-epoch committed source/output/map frontier.
    ///
    /// A worker uses this cursor to install a map before accepting another
    /// source quantum. Returns `None` before the first commit and after reset;
    /// callback snapshots that have advanced beyond the committed frontier are
    /// deliberately not merged into this cursor.
    #[must_use]
    pub fn adoption_frontier(&self) -> Option<crate::PresentationFrontier> {
        let snapshot = self.context.load()?;
        let source = self.rendered_source_end?.0;
        let committed = self.committed.as_ref().filter(|committed| {
            committed.context().session_epoch() == snapshot.context().session_epoch()
        })?;
        Some(
            crate::PresentationFrontier::builder()
                .source(source)
                .output(committed.frontier().output())
                .maybe_warp_map(committed.frontier().warp_map())
                .build(),
        )
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
}
