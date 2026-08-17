use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec, duration_for_frames};
use kithara_platform::sync::Arc;
use kithara_stretch::{StretchOptions, build_backend};

use super::{StretchControls, backend::StretchSnapshot};
#[path = "render.rs"]
mod render;
#[path = "state.rs"]
mod state;

use state::{
    AdmittedSource, DrainFrontier, PrepareCause, PreparedCore, RegionBoundary, RetiredCores,
    TempoCore,
};

use crate::{
    region::RegionPlan,
    traits::{
        OutputCredit, PresentationPoint, TempoBoundaryId, TempoDiscontinuityDebt,
        TempoDiscontinuityStep, TempoEofDebt, TempoEofStep, TempoPrepareRequest, TempoStage,
        TempoStep,
    },
};

/// Resident late-presentation time-stretch stage driven by shared
/// [`StretchControls`]:
///
/// - key-lock **on**: drives the backend with the inverse stretch factor
///   (`1 / speed`) and pitch held at `1.0`;
/// - key-lock **off**: drives the same stretch factor and sets pitch to
///   `speed` for vinyl-style playback.
///
/// At unity speed with no region plan the slot is a byte-identical
/// passthrough, so default playback keeps the old no-DSP behavior.
///
/// An optional [`RegionPlan`] maps source positions to live ratio corrections.
/// A real correction boundary retains future source, prepares a fresh core
/// off-RT, and drains the old core before an allocation-free commit. Adjacent
/// equal-ratio regions stay on the resident core without a boundary cost.
pub struct TimeStretchProcessor {
    controls: Arc<StretchControls>,
    active: Option<TempoCore>,
    prepared: Option<PreparedCore>,
    retired: Option<RetiredCores>,
    next_boundary: u64,
    admitted: Option<AdmittedSource>,
    last_input_meta: Option<PcmMeta>,
    pool: PcmPool,
    requested_spec: PcmSpec,
    eof_offset: usize,
    eof_pending: bool,
    discontinuity_pending: bool,
    drain_frontier: Option<DrainFrontier>,
    region_boundary: Option<RegionBoundary>,
    failed: bool,
}

impl TimeStretchProcessor {
    /// Floor for the shared playback speed before inverting to a stretch
    /// factor. At `speed = 0.05` the stretch is already 20x, beyond which
    /// time-stretch quality collapses, so there is no point clamping lower.
    const MIN_SPEED: f32 = 0.05;
    const PRESENTATION_FRAMES: usize = 512;
    const MAX_STRETCH: usize = 20;
    const BACKEND_INPUT_FRAMES: usize = Self::PRESENTATION_FRAMES / Self::MAX_STRETCH;
    /// Re-apply the stretch ratio to the backend only when it moves this much.
    const RATIO_EPS: f64 = 1e-4;

    /// Build the slot at the source `spec`, driven by the shared `controls`.
    pub fn new(controls: Arc<StretchControls>, spec: PcmSpec, pool: PcmPool) -> Self {
        Self {
            active: None,
            prepared: None,
            retired: None,
            next_boundary: 0,
            admitted: None,
            controls,
            pool,
            requested_spec: spec,
            last_input_meta: None,
            eof_offset: 0,
            eof_pending: false,
            discontinuity_pending: false,
            drain_frontier: None,
            region_boundary: None,
            failed: false,
        }
    }

    fn build_core(&self, snapshot: StretchSnapshot, spec: PcmSpec) -> DecodeResult<TempoCore> {
        if spec.channels == 0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage cannot adopt a zero-channel PCM spec",
            });
        }
        if !snapshot.speed.is_finite() || snapshot.speed <= 0.0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage cannot adopt an invalid playback speed",
            });
        }
        let bypass = snapshot.plan.is_none()
            && (snapshot.speed.max(Self::MIN_SPEED) - 1.0).abs() <= f32::EPSILON;
        if bypass {
            return Ok(TempoCore {
                backend: None,
                scratch: Vec::new(),
                snapshot,
                spec,
                region: None,
                applied_pitch: f64::NAN,
                applied_stretch: f64::NAN,
                source_frames_pushed: 0,
                processing: false,
            });
        }
        let options = Self::options_for(spec, &self.pool);
        let backend = build_backend(snapshot.backend, &options)
            .map_err(|error| DecodeError::pcm_stream("time-stretch backend construction", error))?;
        let channels = usize::from(spec.channels);
        let scratch_samples =
            (Self::PRESENTATION_FRAMES * channels).max(backend.max_tail_samples());
        Ok(TempoCore {
            backend: Some(backend),
            scratch: Vec::with_capacity(scratch_samples),
            snapshot,
            spec,
            region: None,
            applied_pitch: f64::NAN,
            applied_stretch: f64::NAN,
            source_frames_pushed: 0,
            processing: false,
        })
    }

    fn options_for(spec: PcmSpec, pool: &PcmPool) -> StretchOptions {
        StretchOptions::builder()
            .sample_rate(spec.sample_rate.get())
            .channels(usize::from(spec.channels.max(1)))
            .max_input_frames(Self::BACKEND_INPUT_FRAMES)
            .max_output_frames(Self::PRESENTATION_FRAMES)
            .pool(pool.clone())
            .build()
    }

    fn active(&self) -> DecodeResult<&TempoCore> {
        self.active.as_ref().ok_or(DecodeError::InvalidData {
            detail: "tempo stage has no prepared active core",
        })
    }

    fn active_mut(&mut self) -> DecodeResult<&mut TempoCore> {
        self.active.as_mut().ok_or(DecodeError::InvalidData {
            detail: "tempo stage has no prepared active core",
        })
    }

    fn bypass_for(snapshot: &StretchSnapshot) -> bool {
        snapshot.plan.is_none() && (snapshot.speed.max(Self::MIN_SPEED) - 1.0).abs() <= f32::EPSILON
    }

    fn same_plan(left: &Option<Arc<RegionPlan>>, right: &Option<Arc<RegionPlan>>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }

    fn same_snapshot(left: &StretchSnapshot, right: &StretchSnapshot) -> bool {
        left.revision == right.revision
            && left.backend == right.backend
            && left.keylock == right.keylock
            && left.speed.to_bits() == right.speed.to_bits()
            && Self::same_plan(&left.plan, &right.plan)
    }

    fn channels(&self) -> usize {
        self.active.as_ref().map_or_else(
            || usize::from(self.requested_spec.channels.max(1)),
            TempoCore::channels,
        )
    }

    fn validate_credit(&self, credit: &mut OutputCredit<'_>) -> DecodeResult<()> {
        if credit.channels() != self.channels()
            || credit.max_frames() == 0
            || credit.max_frames() > Self::PRESENTATION_FRAMES
        {
            return Err(DecodeError::InvalidData {
                detail: "tempo output credit has an invalid PCM shape",
            });
        }
        let samples =
            credit
                .max_frames()
                .checked_mul(credit.channels())
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo output credit sample count overflow",
                })?;
        if credit.samples_mut().len() < samples {
            return Err(DecodeError::InvalidData {
                detail: "tempo output credit buffer is shorter than its frame count",
            });
        }
        Ok(())
    }

    fn output_meta(
        &self,
        source: &PcmMeta,
        source_offset: usize,
        output_frames: usize,
    ) -> DecodeResult<PcmMeta> {
        let offset = u64::try_from(source_offset).map_err(|_| DecodeError::InvalidData {
            detail: "tempo source offset exceeds u64",
        })?;
        let mut meta = *source;
        let spec = self.active()?.spec;
        meta.spec = spec;
        meta.frame_offset =
            meta.frame_offset
                .checked_add(offset)
                .ok_or(DecodeError::InvalidData {
                    detail: "tempo source frame offset overflow",
                })?;
        meta.timestamp = meta
            .timestamp
            .checked_add(duration_for_frames(spec.sample_rate.get(), offset))
            .ok_or(DecodeError::InvalidData {
                detail: "tempo source timestamp overflow",
            })?;
        meta.frames = u32::try_from(output_frames).map_err(|_| DecodeError::InvalidData {
            detail: "tempo output frame count exceeds u32",
        })?;
        Ok(meta)
    }

    fn admitted_source_frames(&self) -> u64 {
        self.admitted.as_ref().map_or(0, |source| {
            let remaining = source.chunk.frames() - source.consumed_frames;
            u64::try_from(remaining).unwrap_or(u64::MAX)
        })
    }

    fn active_held_source_frames(&self) -> u64 {
        self.active.as_ref().map_or(0, |core| {
            if !core.processing {
                return 0;
            }
            core.backend.as_ref().map_or(0, |backend| {
                u64::try_from(backend.source_latency_frames())
                    .unwrap_or(u64::MAX)
                    .min(core.source_frames_pushed)
            })
        })
    }

    fn admitted_source_frame(&self) -> DecodeResult<Option<u64>> {
        self.admitted
            .as_ref()
            .map(|source| {
                let consumed = u64::try_from(source.consumed_frames).map_err(|_| {
                    DecodeError::InvalidData {
                        detail: "tempo admitted source cursor exceeds u64",
                    }
                })?;
                source.chunk.meta.frame_offset.checked_add(consumed).ok_or(
                    DecodeError::InvalidData {
                        detail: "tempo admitted source position overflow",
                    },
                )
            })
            .transpose()
    }

    fn region_boundary_ready(&self) -> DecodeResult<bool> {
        if !self
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared.cause == PrepareCause::RegionBoundary)
        {
            return Ok(false);
        }
        let boundary = self.region_boundary.ok_or(DecodeError::InvalidData {
            detail: "tempo prepared region boundary lost its source cursor",
        })?;
        if self.admitted_source_frame()? != Some(boundary.source_frame) {
            return Err(DecodeError::InvalidData {
                detail: "tempo region boundary moved away from its retained source",
            });
        }
        Ok(true)
    }
}

impl TempoStage for TimeStretchProcessor {
    fn release_retired_off_rt(&mut self) {
        if let Some(retired) = self.retired.take() {
            retired.release();
        }
    }

    fn service_off_rt(&mut self, request: TempoPrepareRequest) -> DecodeResult<()> {
        self.release_retired_off_rt();
        let Some(snapshot) = self.controls.try_snapshot() else {
            return Ok(());
        };
        let (requested_spec, requested_cause) = match request {
            TempoPrepareRequest::Current { spec } => (spec, PrepareCause::Current),
            TempoPrepareRequest::DecoderBoundary { spec } => (spec, PrepareCause::DecoderBoundary),
        };
        let (spec, cause) = if self.region_boundary.is_some() {
            (self.active()?.spec, PrepareCause::RegionBoundary)
        } else {
            (requested_spec, requested_cause)
        };
        if self.prepared.as_ref().is_some_and(|prepared| {
            prepared.cause == cause
                && prepared.core.spec == spec
                && Self::same_snapshot(&prepared.core.snapshot, &snapshot)
        }) {
            return Ok(());
        }

        if cause == PrepareCause::Current
            && let Some(active) = &self.active
            && active.spec == spec
            && active.snapshot.backend == snapshot.backend
            && active.bypass() == Self::bypass_for(&snapshot)
        {
            drop(self.prepared.take());
            self.update_active_snapshot(snapshot)?;
            self.requested_spec = spec;
            return Ok(());
        }

        let core = self.build_core(snapshot, spec)?;
        if self.active.is_none() {
            drop(self.prepared.take());
            self.requested_spec = spec;
            self.active = Some(core);
            return Ok(());
        }

        drop(self.prepared.take());
        self.next_boundary = self
            .next_boundary
            .checked_add(1)
            .ok_or(DecodeError::InvalidData {
                detail: "tempo prepared-boundary identity overflow",
            })?;
        self.prepared = Some(PreparedCore {
            cause,
            id: TempoBoundaryId::new(self.next_boundary),
            core,
        });
        Ok(())
    }

    fn prepared_boundary(&self) -> Option<TempoBoundaryId> {
        self.prepared.as_ref().and_then(|prepared| {
            let admission_ready =
                self.admitted.is_none() || prepared.cause == PrepareCause::RegionBoundary;
            (admission_ready && prepared.core.snapshot.revision == self.controls.revision())
                .then_some(prepared.id)
        })
    }

    fn commit_prepared(&mut self, id: TempoBoundaryId) -> DecodeResult<()> {
        if self.prepared.as_ref().map(|prepared| prepared.id) != Some(id) {
            return Err(DecodeError::InvalidData {
                detail: "tempo prepared-boundary identity is stale",
            });
        }
        let retains_region_source = self.region_boundary_ready()?;
        if (self.admitted.is_some() && !retains_region_source)
            || self.eof_pending
            || self.discontinuity_pending
            || self.active_held_source_frames() != 0
        {
            return Err(DecodeError::InvalidData {
                detail: "tempo core committed before the ordered drain completed",
            });
        }
        if self.retired.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "tempo core committed before prior state was retired off-RT",
            });
        }
        let prepared = self.prepared.take().ok_or(DecodeError::InvalidData {
            detail: "tempo stage lost its prepared core",
        })?;
        let active = self.active.take();
        self.requested_spec = prepared.core.spec;
        self.active = Some(prepared.core);
        self.retired = Some(RetiredCores {
            active,
            prepared: None,
        });
        self.last_input_meta = None;
        self.eof_offset = 0;
        self.drain_frontier = None;
        self.region_boundary = None;
        Ok(())
    }

    fn buffered_source_quanta(&self) -> usize {
        usize::from(self.admitted.is_some())
    }

    fn output_spec(&self) -> PcmSpec {
        self.active
            .as_ref()
            .map_or(self.requested_spec, |core| core.spec)
    }

    fn push_source(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        if self.failed || self.eof_pending || self.discontinuity_pending {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage is not accepting source in its current state",
            });
        }
        if self.admitted.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage already owns one source chunk",
            });
        }
        if chunk.spec() != self.output_spec() || chunk.frames() == 0 {
            return Err(DecodeError::InvalidData {
                detail: "tempo source chunk has an invalid PCM shape",
            });
        }
        self.admitted = Some(AdmittedSource {
            chunk,
            consumed_frames: 0,
        });
        Ok(())
    }

    fn render(
        &mut self,
        _point: Option<PresentationPoint>,
        credit: OutputCredit<'_>,
        retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoStep> {
        if self.failed {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage is terminal after a render failure",
            });
        }
        if self.eof_pending || self.discontinuity_pending {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage cannot render source while a drain debt is active",
            });
        }
        if self.active.is_none()
            || (self.admitted.is_none()
                && self
                    .active
                    .as_ref()
                    .is_none_or(|core| core.snapshot.revision != self.controls.revision()))
        {
            return Ok(TempoStep::Preparing);
        }
        self.render_admitted(credit, retire)
    }

    fn held_source_frames(&self) -> u64 {
        let backend = self.drain_frontier.as_ref().map_or_else(
            || self.active_held_source_frames(),
            |frontier| frontier.remaining_source_frames,
        );
        self.admitted_source_frames().saturating_add(backend)
    }

    fn finish_eof(&mut self) -> DecodeResult<TempoEofDebt> {
        if self.failed {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage is terminal after a render failure",
            });
        }
        if self.admitted.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "true EOF was declared before admitted tempo source was rendered",
            });
        }
        self.begin_tempo_drain("time-stretch EOF")?;
        self.eof_pending = true;
        Ok(TempoEofDebt::new())
    }

    fn render_eof(
        &mut self,
        _debt: &mut TempoEofDebt,
        credit: OutputCredit<'_>,
        _retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoEofStep> {
        if !self.eof_pending {
            return Err(DecodeError::InvalidData {
                detail: "tempo EOF renderer has no active debt",
            });
        }
        Ok(match self.render_tempo_drain(credit)? {
            Some((frames, meta)) => TempoEofStep::Rendered { frames, meta },
            None => TempoEofStep::Drained,
        })
    }

    fn begin_discontinuity(&mut self) -> DecodeResult<TempoDiscontinuityDebt> {
        if self.failed {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage is terminal after a render failure",
            });
        }
        let retains_region_source = self.region_boundary_ready()?;
        if self.admitted.is_some() && !retains_region_source {
            return Err(DecodeError::InvalidData {
                detail: "tempo discontinuity began before admitted source was rendered",
            });
        }
        self.begin_tempo_drain("time-stretch discontinuity")?;
        self.discontinuity_pending = true;
        Ok(TempoDiscontinuityDebt::new())
    }

    fn render_discontinuity(
        &mut self,
        _debt: &mut TempoDiscontinuityDebt,
        credit: OutputCredit<'_>,
        _retire: &mut dyn FnMut(PcmChunk),
    ) -> DecodeResult<TempoDiscontinuityStep> {
        if !self.discontinuity_pending {
            return Err(DecodeError::InvalidData {
                detail: "tempo discontinuity renderer has no active debt",
            });
        }
        Ok(match self.render_tempo_drain(credit)? {
            Some((frames, meta)) => TempoDiscontinuityStep::Rendered { frames, meta },
            None => TempoDiscontinuityStep::Drained,
        })
    }

    fn deactivate(&mut self, retire: &mut dyn FnMut(PcmChunk)) -> DecodeResult<()> {
        if self.retired.is_some() {
            return Err(DecodeError::InvalidData {
                detail: "tempo stage deactivated before prior state was retired off-RT",
            });
        }
        if let Some(source) = self.admitted.take() {
            retire(source.chunk);
        }
        let active = self.active.take();
        let prepared = self.prepared.take();
        if active.is_some() || prepared.is_some() {
            self.retired = Some(RetiredCores { active, prepared });
        }
        self.last_input_meta = None;
        self.eof_offset = 0;
        self.eof_pending = false;
        self.discontinuity_pending = false;
        self.drain_frontier = None;
        self.region_boundary = None;
        self.failed = false;
        Ok(())
    }
}

#[cfg(test)]
pub(in crate::effects::timestretch) mod tests;
