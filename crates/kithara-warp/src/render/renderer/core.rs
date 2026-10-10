use std::num::{NonZeroU32, NonZeroUsize};

use kithara_bufpool::{HasPool, PoolRegion, SampleBuffer};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, SourceSpan};
use kithara_stretch::{
    BackendCapabilities, ElasticBackendConfig, ElasticEngine, ElasticError, ElasticRequest,
    StretchKind, build_engine,
};
use kithara_test_macros as kithara;
use num_traits::AsPrimitive;
use tracing::warn;

use super::{
    super::trajectory::{Fraction, Trajectory},
    target::PreparedTarget,
};
use crate::{
    ActiveRegion, RegionPlan, RenderReader, RenderSnapshot, SpeedCurve, WarpConfig,
    WarpRenderError, consts,
};

/// The speed a renderer renders at and the revision that set it.
#[derive(Clone, Copy, Debug)]
pub(in crate::render) struct RateTarget {
    speed: f32,
    revision: u64,
}

impl RateTarget {
    fn new(speed: f32, revision: u64) -> Self {
        Self {
            speed: speed.max(consts::MIN_SPEED),
            revision,
        }
    }

    pub(in crate::render) const fn revision(self) -> u64 {
        self.revision
    }

    pub(in crate::render) const fn speed(self) -> f32 {
        self.speed
    }
}

#[derive(Clone, Copy)]
pub(in crate::render) struct PreparedQuantum {
    pub(in crate::render) source_span: Option<SourceSpan>,
    pub(in crate::render) activation: Option<PreparedActivation>,
    pub(in crate::render) rate: RateTarget,
    pub(in crate::render) speed: f32,
    pub(in crate::render) source_start: u64,
    pub(in crate::render) active_frames: usize,
    pub(in crate::render) frames: usize,
    /// Output frames a quantum that ends on a scheduled frame renders exactly.
    pub(in crate::render) landing_frames: Option<usize>,
}

#[derive(Clone, Copy)]
pub(in crate::render) struct PreparedActivation {
    pub(in crate::render) warm: ElasticRequest,
    pub(in crate::render) history_frames: usize,
}

impl PreparedActivation {
    pub(in crate::render) fn prefix_frames(self) -> Result<usize, ElasticError> {
        self.history_frames
            .checked_add(self.warm.source_frames())
            .ok_or(ElasticError::SampleCountOverflow)
    }
}

/// Source-timeline exact-span time-stretch driven by its render lane.
/// Unity speed without a region plan is a byte-identical passthrough.
#[non_exhaustive]
pub struct WarpRenderer<S> {
    pub(in crate::render) trajectory: Trajectory,
    pub(in crate::render) mapped_render: bool,
    pub(in crate::render) terminal_source_end: Option<u64>,
    pub(in crate::render) projection: Option<super::projected::Projection>,
    pub(in crate::render) retiring_target: Option<super::transition::RetiringTarget>,
    pub(in crate::render) spec: AudioSpec,
    pub(in crate::render) backends: ElasticBackendConfig,
    /// Maximum source frames admitted to one elastic render operation.
    pub(in crate::render) source_block_frames: NonZeroUsize,
    /// Latency-sized pooled output discarded while priming an inactive engine.
    pub(in crate::render) activation_scratch: Option<SampleBuffer>,
    /// Speed the last [`Self::set_speed`] set, with the revision stamped on
    /// every chunk rendered toward it.
    pub(in crate::render) rate: RateTarget,
    pub(in crate::render) committed: Option<RenderSnapshot>,
    /// Consumed input retained until the scheduler shell can resize or recycle
    /// it outside the checked render core.
    pub(in crate::render) deferred_scratch: Option<SampleBuffer>,
    pub(in crate::render) engine: Option<Box<dyn ElasticEngine>>,
    /// Most recent input meta, carried onto each output chunk.
    pub(in crate::render) last_input_meta: Option<AudioChunkInfo>,
    /// Exact source coordinate at which the current output scratch begins.
    pub(in crate::render) output_start_meta: Option<AudioChunkInfo>,
    /// Earliest metadata represented by `pending_source`.
    pub(in crate::render) pending_meta: Option<AudioChunkInfo>,
    /// Source whose cumulative output is still below one representable frame.
    /// Capacity is reserved from the injected pool before the render loop.
    pub(in crate::render) pending_source: Option<SampleBuffer>,
    /// Unity chunk retained while the active backend drains its tail.
    /// Its samples occupy `pending_source` without a copy.
    pub(in crate::render) pending_unity_meta: Option<AudioChunkInfo>,
    /// Region plan the renderer was built with.
    pub(in crate::render) plan: Option<Arc<RegionPlan>>,
    /// Source span and live speed selected by the scheduler for the next render.
    pub(in crate::render) prepared_quantum: Option<PreparedQuantum>,
    /// Region covering the playhead - the lookup cursor. `None` forces a
    /// fresh binary search (first chunk, region exit, seek).
    pub(in crate::render) region: Option<ActiveRegion>,
    /// Maximum output frames between samples of live temporal controls.
    pub(in crate::render) render_quantum_frames: Option<NonZeroUsize>,
    /// Exact decoded-source boundary represented by the latest emitted chunk.
    pub(in crate::render) rendered_source_end: Option<(u64, NonZeroU32)>,
    pub(in crate::render) residency: Option<super::residency::SourceResidency>,
    /// Engine displaced by a checked render failure. The scheduler shell
    /// drops it from `prepare`, outside `produce_tick_rt`.
    pub(in crate::render) retired_engine: Option<Box<dyn ElasticEngine>>,
    /// Interleaved output scratch prepared by the scheduler shell. A produced
    /// chunk takes this buffer; the consumed input becomes its replacement.
    pub(in crate::render) scratch: Option<SampleBuffer>,
    pub(in crate::render) pools: PoolRegion<S>,
    pub(in crate::render) context: RenderReader,
    /// Engine kind currently prepared by the scheduler shell.
    pub(in crate::render) current_kind: StretchKind,
    /// Backend the lane last asked for; the scheduler shell prepares its engine.
    pub(in crate::render) requested_kind: StretchKind,
    /// Whether the lane last asked for a keylock engine.
    pub(in crate::render) requested_keylock: bool,
    /// Whether previous input ran through the backend. Drives a clean backend
    /// reset when the renderer returns to unity passthrough.
    pub(in crate::render) active: bool,
    pub(in crate::render) backend_transition_pending: bool,
    /// A speed set while the engine runs: its tail retires into the
    /// residency's replacement and the engine re-primes on the speed's frame.
    pub(in crate::render) reprime_pending: bool,
    /// Source frame the engine reads next while it trails the decoded frontier
    /// after a prime; the residency holds every frame from it on.
    pub(in crate::render) resident_feed: Option<u64>,
    pub(in crate::render) current_keylock: bool,
    /// One scheduler-shell rebuild requested for a re-prime or after a checked
    /// engine failure. The intent is consumed even when preparation fails.
    pub(in crate::render) rebuild_pending: bool,
    /// Reset requested by seek or a return to unity passthrough. The scheduler
    /// shell performs it outside the checked render core.
    pub(in crate::render) reset_pending: bool,
    /// Last pitch factor pushed to the backend; avoids redundant updates.
    pub(in crate::render) applied_pitch: f64,
    /// Fractional output frames retained across exact-span requests.
    pub(in crate::render) output_remainder: f64,
    /// Warm source consumed while priming but not yet represented by output.
    pub(in crate::render) primed_source_debt: u64,
    /// Source frames admitted since the last renderer reset.
    pub(in crate::render) source_frames_admitted: u64,
}

impl<S> WarpRenderer<S>
where
    S: HasPool<f32>,
{
    pub(in crate::render) const MAX_OUTPUT_FRAMES: usize = 163_840;
    pub(in crate::render) const OUTPUT_ROUNDING_MARGIN: f64 = 0.5;
    /// Build the slot at the source `spec` from the construction values of `config`.
    pub(crate) fn new(
        config: &WarpConfig,
        context: RenderReader,
        spec: AudioSpec,
        pools: PoolRegion<S>,
    ) -> Self {
        let requested_kind = config.backend();
        let requested_keylock = config.keylock();
        let (current_kind, current_keylock) = Self::stretch_for(requested_kind, requested_keylock);
        let plan = config.region_plan().clone();
        let speed = config.speed();
        let target = if plan.is_some()
            && !current_kind
                .capabilities()
                .contains(BackendCapabilities::RATE)
        {
            Self::config_for(
                current_kind,
                config.backends(),
                config.source_block_frames(),
                spec,
                &pools,
            )
            .and_then(build_engine)
            .map(|engine| PreparedTarget {
                engine: Some(engine),
                ..PreparedTarget::default()
            })
        } else {
            Self::prepare_target(
                (current_kind, current_keylock),
                config.backends(),
                config.source_block_frames(),
                spec,
                &pools,
                PreparedTarget::default(),
                true,
            )
        }
        .unwrap_or_else(|error| {
            warn!(%current_kind, %error, "time-stretch engine preparation failed");
            PreparedTarget::default()
        });
        Self {
            trajectory: Trajectory::new(speed),
            mapped_render: false,
            terminal_source_end: None,
            projection: target.projection,
            retiring_target: None,
            context,
            residency: target.residency,
            committed: None,
            backends: config.backends(),
            engine: target.engine,
            retired_engine: None,
            current_kind,
            current_keylock,
            requested_kind,
            requested_keylock,
            backend_transition_pending: false,
            reprime_pending: false,
            resident_feed: None,
            pools,
            spec,
            source_block_frames: config.source_block_frames(),
            render_quantum_frames: config.render_quantum_frames(),
            prepared_quantum: None,
            applied_pitch: f64::NAN,
            rate: RateTarget::new(speed, 0),
            active: false,
            output_remainder: 0.0,
            pending_source: target.pending_source,
            pending_meta: None,
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
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    /// Output latency of the installed engine, even before activation.
    ///
    /// Returns zero when no engine is installed. During a transition this is
    /// the outgoing engine's latency; use [`Self::prepare_engine_latency`] for
    /// the requested engine's after-state.
    #[must_use]
    pub fn engine_latency(&self) -> FrameCount {
        if self.keylocked_unity() && !self.active {
            return FrameCount::new(0);
        }
        FrameCount::new(self.engine.as_ref().map_or(0, |engine| {
            let stages = self
                .projection
                .as_ref()
                .map_or(1, |projection| projection.stages);
            engine
                .capabilities()
                .latency()
                .second()
                .saturating_mul(stages)
        }))
    }

    /// Prepare the requested engine and report its output latency.
    ///
    /// Retires an outgoing engine into the crossfade without emitting PCM or
    /// advancing the rendered source frontier, then installs the target.
    ///
    /// # Errors
    /// Returns [`WarpRenderError::NeedsService`] if previously queued unity
    /// output must be consumed before applying the next batch, or the engine's
    /// admission error if preparation fails.
    pub fn prepare_engine_latency(
        &mut self,
        spec: AudioSpec,
    ) -> Result<FrameCount, WarpRenderError> {
        self.prepare(spec);
        if self.pending_unity_meta.is_some() {
            return Err(WarpRenderError::NeedsService);
        }
        if self.reprime_pending || self.backend_transition_pending {
            self.retire_for_reprime()?;
            self.prepare(spec);
        }
        if self.transition_pending()
            || self.stretch_target() != (self.current_kind, self.current_keylock)
        {
            return Err(WarpRenderError::NeedsService);
        }
        if self.engine.is_none() {
            return Err(ElasticError::EnginePreparation("engine is unavailable").into());
        }
        if !self.active && self.projection.is_some() {
            let stages = self.projection_stages()?;
            if let Some(projection) = self.projection.as_mut() {
                projection.stages = stages;
            }
        }
        Ok(self.engine_latency())
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

    /// Push each distinct pitch to the backend without rounding curve steps.
    pub(in crate::render) fn apply_pitch(&mut self, pitch: f64) -> Result<(), ElasticError> {
        if pitch.to_bits() == self.applied_pitch.to_bits() {
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

    pub(in crate::render) fn clear_pending_source(&mut self) {
        if let Some(source) = self.pending_source.as_mut() {
            source.clear();
        }
        self.pending_meta = None;
        self.pending_unity_meta = None;
    }

    pub(in crate::render) fn clear_render_state(&mut self) {
        if let Some(residency) = self.residency.as_mut() {
            residency.clear();
        }
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
        self.reprime_pending = false;
        self.resident_feed = None;
        self.region = None;
        self.trajectory.reset();
        self.mapped_render = false;
        self.terminal_source_end = None;
        self.retiring_target = None;
    }

    /// The single place a render becomes the committed one, so every committed
    /// render publishes the session axis it fixed regardless of the path that
    /// produced it.
    pub(in crate::render) fn commit(
        &mut self,
        committed: RenderSnapshot,
        output_start: i64,
        source_start: u64,
    ) {
        kithara::probe_event!(
            render_committed,
            session_epoch = u64::from(committed.context().output().session_epoch()),
            transport_revision = committed
                .context()
                .output()
                .transport_revision()
                .map_or(0, u64::from),
            output_start,
            source_start,
            source_end = committed.frontier().source()
        );
        self.committed = Some(committed);
    }

    pub(in crate::render) fn commit_rate_render(
        &mut self,
        snapshot: Option<RenderSnapshot>,
        output: &AudioChunk,
        applied_rate: f32,
    ) {
        let output_frames = output.frames();
        let request_revision = output.meta.render_revision;
        let applied_rate: f32 = if let Some(span) = output.meta.source_span {
            let rate = span
                .source_ratio_at(0)
                .zip(span.source_ratio_at(span.output_frames()))
                .and_then(|(start, end)| {
                    Fraction {
                        numerator: end.0,
                        denominator: end.1,
                    }
                    .sub(Fraction {
                        numerator: start.0,
                        denominator: start.1,
                    })
                })
                .and_then(Fraction::as_f64);
            let Some(rate) = rate else {
                warn!("rendered source mapping rate is unrepresentable");
                return;
            };
            let frames: f64 = span.output_frames().as_();
            (rate / frames).as_()
        } else {
            applied_rate
        };
        let Some(snapshot) = snapshot else {
            return;
        };
        let snapshot = snapshot.bind_output_identity(
            output
                .meta
                .mapping_revision
                .map(crate::WarpMapRevision::from),
        );
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
        self.commit(committed, session_frame, source_start);
    }

    pub(in crate::render) fn commit_render(
        &mut self,
        snapshot: Option<RenderSnapshot>,
        output: &AudioChunk,
    ) {
        let output_frames = output.frames();
        let Some(snapshot) = snapshot else {
            return;
        };
        let snapshot = snapshot.bind_output_identity(
            output
                .meta
                .mapping_revision
                .map(crate::WarpMapRevision::from),
        );
        let Some((committed, output_start, source_start, _)) =
            self.next_render_snapshot(snapshot, output_frames)
        else {
            return;
        };
        self.commit(committed, output_start, source_start);
    }

    pub(in crate::render) fn defer_scratch(&mut self, replacement: Option<SampleBuffer>) {
        if let Some(replacement) = replacement {
            debug_assert!(self.deferred_scratch.is_none());
            self.deferred_scratch = Some(replacement);
        }
    }

    /// Whether the next quantum would engage an engine other than the one
    /// the lane asks for, so the scheduler shell must prepare that one first.
    pub(in crate::render) fn engine_outdated(&self) -> bool {
        !self.active
            && (!self.requires_staging() || !self.unity_passthrough(self.rate.speed()))
            && self.stretch_target() != (self.current_kind, self.current_keylock)
    }

    pub(in crate::render) fn held_source_frames(&self) -> u64 {
        if !self.active {
            return 0;
        }
        let pending = u64::try_from(self.pending_frames(usize::from(self.spec.channels.max(1))))
            .unwrap_or(u64::MAX);
        let backend_admitted = self.source_frames_admitted.saturating_sub(pending);
        let latency = self
            .engine
            .as_ref()
            .map_or(0, |engine| engine.capabilities().latency().first());
        let backend_held = u64::try_from(latency)
            .unwrap_or(u64::MAX)
            .min(backend_admitted);
        pending
            .saturating_add(backend_held)
            .saturating_add(self.primed_source_debt)
    }
}

impl<S: HasPool<f32>> WarpRenderer<S> {
    pub(in crate::render) fn meta_at_frame(
        meta: AudioChunkInfo,
        frame_offset: u64,
    ) -> AudioChunkInfo {
        let mut start = meta;
        let delta = frame_offset.abs_diff(meta.frame_offset);
        let span = meta
            .spec
            .duration_for(delta)
            .unwrap_or(Duration::from_nanos(u64::MAX));
        start.frame_offset = frame_offset;
        start.timestamp = if frame_offset < meta.frame_offset {
            meta.timestamp.saturating_sub(span)
        } else {
            meta.timestamp.saturating_add(span)
        };
        if delta > 0 {
            start.source_byte_offset = None;
            start.source_bytes = 0;
        }
        start
    }

    pub(super) fn next_render_snapshot(
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
                previous.context().output().session_epoch()
                    == snapshot.context().output().session_epoch()
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

    pub(in crate::render) fn pending_frames(&self, channels: usize) -> usize {
        if self.pending_unity_meta.is_some() {
            return 0;
        }
        self.pending_source
            .as_deref()
            .map_or(0, |source| source.len() / channels)
    }

    pub(in crate::render) fn record_rendered_source_end(
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

    pub(in crate::render) fn map_output(
        &mut self,
        output: &mut AudioChunk,
    ) -> Result<(), ElasticError> {
        let span = output
            .meta
            .source_span
            .ok_or(ElasticError::EnginePreparation(
                "rendered output has no exact source mapping",
            ))?
            .with_render_revision(output.meta.render_revision)
            .with_mapping_revision(output.meta.mapping_revision);
        output.meta.frame_offset = span.start();
        output.meta.timestamp = span
            .position_at(0)
            .ok_or(ElasticError::SampleCountOverflow)?;
        output.meta.end_timestamp = span
            .position_at(span.output_frames())
            .ok_or(ElasticError::SampleCountOverflow)?;
        output.meta.source_span = Some(span);
        self.residency
            .as_mut()
            .ok_or(ElasticError::PoolCapacity)?
            .remember_mapping(span)?;
        self.trajectory.advance(span)?;
        if let Some(plan) = self.plan.as_ref()
            && plan.region_at(span.start()) != plan.region_at(span.end())
        {
            self.trajectory.snap_position()?;
        }
        self.rendered_source_end = Some((span.end(), span.sample_rate()));
        self.rate = RateTarget::new(self.trajectory.speed()?, self.rate.revision());
        Ok(())
    }

    /// Region covering `frame`, plus whether the playhead just crossed out
    /// of a previously resolved region (a plan boundary or a seek).
    pub(in crate::render) fn region_for(&mut self, frame: u64) -> ActiveRegion {
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

    /// Whether the applied backend changes rate and needs worker staging.
    #[must_use]
    pub const fn requires_staging(&self) -> bool {
        self.current_kind
            .capabilities()
            .contains(BackendCapabilities::RATE)
    }

    /// Re-primes a running engine on this frame when the requested one differs
    /// from it, and drops a quantum prepared before.
    pub(super) fn retarget_engine(&mut self) {
        let target = self.stretch_target();
        let changed = target != (self.current_kind, self.current_keylock);
        if target.0.capabilities().contains(BackendCapabilities::RATE) {
            self.reprime_pending |= self.active && changed;
        } else {
            self.reprime_pending = false;
            self.backend_transition_pending |= self.active && changed;
        }
        self.prepared_quantum = None;
    }

    pub(in crate::render) fn retire_engine(&mut self) {
        debug_assert!(self.retired_engine.is_none());
        self.retired_engine = self.engine.take();
        self.rebuild_pending = true;
    }

    /// Render from the next prepared quantum on with `kind`'s engine. A running
    /// engine retires its tail into a crossfade and the new engine re-primes
    /// from the source history on this frame.
    pub fn set_backend(&mut self, kind: StretchKind) {
        self.requested_kind = kind;
        self.retarget_engine();
    }

    /// Render from the next prepared quantum on with a keylock engine when `on`
    /// and the backend has one, switching engines as [`Self::set_backend`] does.
    pub fn set_keylock(&mut self, on: bool) {
        self.requested_keylock = on;
        self.retarget_engine();
    }

    /// Render from the next prepared quantum on at the speed `curve` holds,
    /// stamping `revision` on every chunk rendered toward it. A quantum
    /// prepared before is dropped, so the next one is planned at this speed.
    /// Replaces the remaining curve at the next output frame, preserving phase.
    ///
    /// # Errors
    /// Rejects nonfinite or unsupported speeds, unordered steps, and mappings
    /// that exceed the checked rational representation. Rejection changes no state.
    pub fn set_speed(&mut self, curve: SpeedCurve, revision: u64) -> Result<(), WarpRenderError> {
        let target = RateTarget::new(self.trajectory.replace(curve)?, revision);
        self.reprime_pending |= self.active
            && (self.mapped_render || !self.unity_passthrough(target.speed()))
            && self
                .stretch_target()
                .0
                .capabilities()
                .contains(BackendCapabilities::RATE)
            && self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.capabilities().latency().first() > 0);
        self.rate = target;
        self.prepared_quantum = None;
        Ok(())
    }

    /// The engine `kind` and `keylock` ask for: keylock only where the backend
    /// has it.
    pub(super) fn stretch_for(kind: StretchKind, keylock: bool) -> (StretchKind, bool) {
        (
            kind,
            keylock && kind.capabilities().contains(BackendCapabilities::KEYLOCK),
        )
    }

    /// The engine the lane last asked for.
    pub(in crate::render) fn stretch_target(&self) -> (StretchKind, bool) {
        Self::stretch_for(self.requested_kind, self.requested_keylock)
    }

    /// Whether a live active-to-unity transition still owns queued samples.
    #[must_use]
    pub const fn transition_pending(&self) -> bool {
        self.pending_unity_meta.is_some() || self.backend_transition_pending || self.reprime_pending
    }

    pub(in crate::render) fn unity_passthrough(&self, speed: f32) -> bool {
        !self.requires_staging() || (self.plan.is_none() && (speed - 1.0).abs() <= f32::EPSILON)
    }

    pub(in crate::render) fn keylocked_unity(&self) -> bool {
        self.current_keylock && self.plan.is_none() && self.trajectory.constant_unity()
    }
}
