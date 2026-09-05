use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunkInfo, AudioSpec, FrameCount};
use kithara_stretch::{ElasticBackendConfig, ElasticEngine, ElasticError, StretchKind};
use kithara_test_macros as kithara;
use tracing::warn;

use crate::{ActiveRegion, RegionPlan, RenderReader, RenderSnapshot, StretchControls, WarpConfig};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct PreparedQuantum {
    pub(super) frames: usize,
    pub(super) speed: f32,
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
    pub(super) pools: PoolRegion<S>,
    pub(super) spec: AudioSpec,
    /// Maximum output frames between samples of live temporal controls.
    pub(super) backends: ElasticBackendConfig,
    pub(super) render_quantum_frames: Option<NonZeroUsize>,
    /// Source span and live speed selected by the scheduler for the next render.
    pub(super) prepared_quantum: Option<PreparedQuantum>,
    /// Engine kind currently prepared by the scheduler shell.
    pub(super) current_kind: StretchKind,
    /// Interleaved output scratch prepared by the scheduler shell. A produced
    /// chunk takes this buffer; the consumed input becomes its replacement.
    pub(super) scratch: Option<SampleBuffer>,
    /// Consumed input retained until the scheduler shell can resize or recycle
    /// it outside the checked render core.
    pub(super) deferred_scratch: Option<SampleBuffer>,
    /// Whether previous input ran through the backend. Drives a clean backend
    /// reset when the renderer returns to unity passthrough.
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
    /// Unity chunk retained while the active backend drains its tail.
    /// Its samples occupy `pending_source` without a copy.
    pub(super) pending_unity_meta: Option<AudioChunkInfo>,
    /// Exact decoded-source boundary represented by the latest emitted chunk.
    pub(super) rendered_source_end: Option<(u64, NonZeroU32)>,
    /// Source frames admitted since the last renderer reset.
    pub(super) source_frames_admitted: u64,
    /// Reset requested by seek or a return to unity passthrough. The scheduler
    /// shell performs it outside the checked render core.
    pub(super) reset_pending: bool,
    /// One scheduler-shell rebuild requested after a checked engine failure.
    /// The intent is consumed even when preparation fails.
    pub(super) rebuild_pending: bool,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(super) const MAX_OUTPUT_FRAMES: usize = 163_840;
    pub(super) const MAX_SOURCE_FRAMES: usize = 8192;
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
        let backends = config.backends();
        let target = Self::prepare_target(current_kind, backends, spec, &pools, None, None);
        Self {
            context,
            committed: None,
            engine: target.engine,
            retired_engine: None,
            current_kind,
            controls,
            pools,
            spec,
            backends,
            render_quantum_frames: config.render_quantum_frames(),
            prepared_quantum: None,
            applied_pitch: f64::NAN,
            active: false,
            output_remainder: 0.0,
            pending_source: target.pending_source,
            pending_meta: None,
            pending_unity_meta: None,
            rendered_source_end: None,
            source_frames_admitted: 0,
            reset_pending: false,
            rebuild_pending: false,
            last_input_meta: None,
            output_start_meta: None,
            scratch: target.scratch,
            deferred_scratch: None,
            plan,
            region: None,
        }
    }

    /// Select the next source span that fits the configured output quantum.
    pub fn prepare_quantum(
        &mut self,
        meta: AudioChunkInfo,
        remaining: usize,
    ) -> Option<FrameCount> {
        self.sync_plan();
        let speed = self.controls.speed();
        match self.source_frames_for_quantum(meta, remaining, speed) {
            Ok(frames) => {
                self.prepared_quantum = Some(PreparedQuantum { frames, speed });
                Some(FrameCount::new(frames))
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
        self.prepared_quantum = Some(prepared);
        Some(FrameCount::new(frames))
    }

    /// Whether this target has elastic DSP and needs worker staging.
    #[must_use]
    pub const fn requires_staging(&self) -> bool {
        true
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
        self.pending_unity_meta = None;
    }

    pub(super) fn retire_engine(&mut self) {
        debug_assert!(self.retired_engine.is_none());
        self.retired_engine = self.engine.take();
        self.rebuild_pending = true;
    }

    pub(super) fn clear_render_state(&mut self) {
        if let Some(scratch) = self.scratch.as_mut() {
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
        self.active = false;
        self.region = None;
    }

    pub(super) fn defer_scratch(&mut self, replacement: Option<SampleBuffer>) {
        if let Some(replacement) = replacement {
            debug_assert!(self.deferred_scratch.is_none());
            self.deferred_scratch = Some(replacement);
        }
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
        }
    }

    pub(super) fn unity_passthrough(&self, speed: f32) -> bool {
        self.plan.is_none() && (speed - 1.0).abs() <= f32::EPSILON
    }

    pub(super) fn pending_frames(&self, channels: usize) -> usize {
        if self.transition_pending() {
            return 0;
        }
        self.pending_source
            .as_deref()
            .map_or(0, |source| source.len() / channels)
    }

    /// Whether a live active-to-unity transition still owns queued samples.
    #[must_use]
    pub const fn transition_pending(&self) -> bool {
        self.pending_unity_meta.is_some()
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
        pending.saturating_add(backend_held)
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

    pub(super) fn commit_render(&mut self, snapshot: Option<RenderSnapshot>, output_frames: usize) {
        let Some(snapshot) = snapshot else {
            return;
        };
        if !self.context.is_current(&snapshot) {
            return;
        }
        let Some((source, _)) = self.rendered_source_end else {
            return;
        };
        let source_start = snapshot.frontier().source();
        let output_start = i64::from(snapshot.frontier().output());
        if let Some(committed) = snapshot.advance(self.committed.as_ref(), source, output_frames) {
            self.render_committed(committed, source_start, output_start);
        }
    }

    #[kithara::probe(
        session_epoch = u64::from(committed.context().session_epoch()),
        transport_revision = committed.context().transport_revision().map_or(0, u64::from),
        output_start,
        output_end = i64::from(committed.frontier().output()),
        source_start,
        source_end = committed.frontier().source()
    )]
    fn render_committed(
        &mut self,
        committed: RenderSnapshot,
        source_start: u64,
        output_start: i64,
    ) {
        self.committed = Some(committed);
    }

    /// Exact decoded-source boundary represented by the latest emitted samples.
    #[must_use]
    pub const fn rendered_source_end(&self) -> Option<(u64, NonZeroU32)> {
        self.rendered_source_end
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
