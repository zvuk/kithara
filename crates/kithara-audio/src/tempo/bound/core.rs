use kithara_bufpool::PcmPool;
use kithara_decode::{DecodeError, DecodeResult, PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stretch::{
    ElasticCapabilities, ElasticCursor, ElasticEngine, ElasticLatency, ElasticPriming,
    ElasticRequest, ElasticSpan, ElasticSpanConfig, ElasticSpanPlan,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::BoundError;
use crate::{
    musical::{SessionBeat, SourceSchedule},
    region::{ActiveRegion, RegionPlan},
    tempo::slot::TempoBinding,
    traits::AudioEffect,
};

trait PrimingLifecycle: Sized {
    fn new(latency: ElasticLatency, spec: PcmSpec) -> Self;
    fn activate_resident(&mut self);
    fn deactivate(&mut self);
    fn reset(&mut self);
}

trait PrimingOps {
    fn retained_source_frames(&self, request: ElasticRequest) -> Result<usize, BoundError>;
    fn prime_if_needed(&mut self, start: u64, request: ElasticRequest) -> Result<(), BoundError>;
}

#[cfg(not(test))]
mod priming {
    use kithara_decode::PcmSpec;
    use kithara_stretch::{ElasticLatency, ElasticPriming, ElasticRequest};

    use super::{BoundError, BoundRenderer, PrimingLifecycle, PrimingOps};

    pub(super) struct State;

    impl PrimingLifecycle for State {
        fn new(_latency: ElasticLatency, _spec: PcmSpec) -> Self {
            Self
        }

        fn activate_resident(&mut self) {}

        fn deactivate(&mut self) {}

        fn reset(&mut self) {}
    }

    impl<E: ElasticPriming> PrimingOps for BoundRenderer<E> {
        fn retained_source_frames(&self, _request: ElasticRequest) -> Result<usize, BoundError> {
            Ok(0)
        }

        fn prime_if_needed(
            &mut self,
            _start: u64,
            _request: ElasticRequest,
        ) -> Result<(), BoundError> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod priming {
    use kithara_bufpool::PcmPool;
    use kithara_decode::PcmSpec;
    use kithara_platform::sync::Arc;
    use kithara_stretch::{
        ElasticEngine, ElasticLatency, ElasticPriming, ElasticRequest, ElasticSpanConfig,
    };
    use num_traits::ToPrimitive;

    use super::{BoundError, BoundRenderer, PrimingLifecycle, PrimingOps};
    use crate::{
        musical::{SessionBeat, SourceSchedule},
        tempo::slot::TempoBinding,
    };

    pub(super) struct State {
        primed: bool,
        retain_for_priming: bool,
        discarded: Vec<f32>,
        history: Vec<f32>,
        warmup: Vec<f32>,
    }

    impl PrimingLifecycle for State {
        fn new(latency: ElasticLatency, spec: PcmSpec) -> Self {
            let channels = usize::from(spec.channels.max(1));
            Self {
                primed: true,
                retain_for_priming: false,
                discarded: vec![0.0; latency.output_frames().saturating_mul(channels)],
                history: vec![0.0; latency.source_frames().saturating_mul(channels)],
                warmup: vec![
                    0.0;
                    latency
                        .output_frames()
                        .saturating_mul(2)
                        .saturating_mul(channels)
                ],
            }
        }

        fn activate_resident(&mut self) {
            self.retain_for_priming = false;
            self.primed = true;
        }

        fn deactivate(&mut self) {
            self.primed = true;
        }

        fn reset(&mut self) {
            self.primed = !self.retain_for_priming;
        }
    }

    impl<E: ElasticPriming> BoundRenderer<E> {
        pub(crate) fn new(
            schedule: Arc<SourceSchedule>,
            session_origin: SessionBeat,
            engine: E,
            span_config: ElasticSpanConfig,
            spec: PcmSpec,
            pool: PcmPool,
        ) -> Result<Self, BoundError> {
            let mut renderer = Self::resident(engine, span_config, spec, pool);
            renderer.bind(Arc::new(TempoBinding::new_for_renderer(
                schedule,
                session_origin,
            )))?;
            Ok(renderer)
        }

        pub(crate) fn bind(&mut self, binding: Arc<TempoBinding>) -> Result<(), BoundError> {
            ElasticEngine::reset(&mut self.engine)?;
            self.priming.retain_for_priming = true;
            self.priming.primed = false;
            self.install_binding(binding)
        }

        fn copy_preceding(
            pending: &[f32],
            pending_start: u64,
            channels: usize,
            end: u64,
            frames: usize,
            output: &mut [f32],
        ) -> Result<(), BoundError> {
            let frame_count = frames.to_u64().ok_or(BoundError::BlockOverflow)?;
            let available_frames = end.min(frame_count);
            let start = end.saturating_sub(frame_count);
            let samples = frames
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            if output.len() != samples {
                return Err(BoundError::BlockOverflow);
            }
            output.fill(0.0);
            if available_frames == 0 {
                return Ok(());
            }
            if start < pending_start {
                return Err(BoundError::BehindWindow {
                    requested: i64::try_from(start).map_err(|_| BoundError::BlockOverflow)?,
                    available: pending_start,
                });
            }
            let source_start = usize::try_from(start - pending_start)
                .map_err(|_| BoundError::BlockOverflow)?
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            let source_samples = usize::try_from(available_frames)
                .map_err(|_| BoundError::BlockOverflow)?
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            let source_end = source_start
                .checked_add(source_samples)
                .ok_or(BoundError::BlockOverflow)?;
            let source = pending
                .get(source_start..source_end)
                .ok_or(BoundError::BlockOverflow)?;
            let copy_start = samples
                .checked_sub(source_samples)
                .ok_or(BoundError::BlockOverflow)?;
            output[copy_start..].copy_from_slice(source);
            Ok(())
        }

        fn priming_request(&self, request: ElasticRequest) -> Result<ElasticRequest, BoundError> {
            let latency = self.capabilities.latency();
            let source_frames = request
                .source_frames()
                .to_f64()
                .zip(request.output_frames().to_f64())
                .zip(latency.output_frames().to_f64())
                .and_then(|((source, output), warmup)| {
                    (source / output * warmup).floor().to_usize()
                })
                .ok_or(BoundError::BlockOverflow)?;
            Ok(ElasticRequest::new(source_frames, latency.output_frames())?)
        }

        fn retained_source_frames_for_priming(
            &self,
            request: ElasticRequest,
        ) -> Result<usize, BoundError> {
            if !self.priming.retain_for_priming {
                return Ok(0);
            }
            self.capabilities
                .latency()
                .source_frames()
                .checked_add(self.priming_request(request)?.source_frames())
                .ok_or(BoundError::BlockOverflow)
        }

        fn prime(&mut self, start: u64, request: ElasticRequest) -> Result<(), BoundError> {
            let latency = self.capabilities.latency();
            let warmup_frames = request
                .source_frames()
                .to_u64()
                .ok_or(BoundError::BlockOverflow)?;
            let channels = self.channels();
            Self::copy_preceding(
                &self.pending,
                self.pending_start,
                channels,
                start.saturating_sub(warmup_frames),
                latency.source_frames(),
                &mut self.priming.history,
            )?;
            let warmup_samples = request
                .source_frames()
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            let warmup = self
                .priming
                .warmup
                .get_mut(..warmup_samples)
                .ok_or(BoundError::BlockOverflow)?;
            Self::copy_preceding(
                &self.pending,
                self.pending_start,
                channels,
                start,
                request.source_frames(),
                warmup,
            )?;
            let discarded_samples = latency
                .output_frames()
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            let discarded = self
                .priming
                .discarded
                .get_mut(..discarded_samples)
                .ok_or(BoundError::BlockOverflow)?;
            discarded.fill(0.0);
            self.engine
                .prime(request, &self.priming.history, warmup, discarded)?;
            self.priming.primed = true;
            Ok(())
        }

        fn prime_if_needed_inner(
            &mut self,
            start: u64,
            request: ElasticRequest,
        ) -> Result<(), BoundError> {
            if !self.priming.primed {
                let priming = self.priming_request(request)?;
                self.prime(start, priming)?;
            }
            Ok(())
        }
    }

    impl<E: ElasticPriming> PrimingOps for BoundRenderer<E> {
        fn retained_source_frames(&self, request: ElasticRequest) -> Result<usize, BoundError> {
            self.retained_source_frames_for_priming(request)
        }

        fn prime_if_needed(
            &mut self,
            start: u64,
            request: ElasticRequest,
        ) -> Result<(), BoundError> {
            self.prime_if_needed_inner(start, request)
        }
    }
}

#[path = "streaming.rs"]
mod streaming;

/// Exact-span tempo slot for a deck bound to the session grid.
///
/// The streaming slot is push-driven: a chunk goes in and whatever the backend
/// renders comes out. This slot is the inverse. It chooses the output span
/// itself, asks the schedule which source span that span is due to consume, and
/// renders exactly those frames through an [`ElasticEngine`]. Output frame `n`
/// therefore lands on the session frame the binding placed it at, with no
/// accumulated rounding: [`ElasticSpanPlan`] quantizes each block's endpoints
/// and carries the fractional remainder in its [`ElasticCursor`].
///
/// The slot is forward-only. It retains the bounded source tail required to
/// prime from the real audio preceding a span.
pub(crate) struct BoundRenderer<E: ElasticPriming> {
    schedule: Option<Arc<SourceSchedule>>,
    /// Session beat aligned with this deck's output frame zero.
    session_origin: Option<SessionBeat>,
    binding: Option<Arc<TempoBinding>>,
    engine: E,
    capabilities: ElasticCapabilities,
    span_config: ElasticSpanConfig,
    cursor: Option<ElasticCursor>,
    /// Interleaved source frames admitted but not yet consumed by the engine.
    pending: Vec<f32>,
    /// Integer source frame of the first pending frame.
    pending_start: u64,
    /// Source frames consumed by the engine since the last reset.
    consumed: u64,
    /// Next output frame to plan.
    output_frame: u64,
    priming: priming::State,
    /// Session beats this deck has advanced since its start.
    ///
    /// The deck's own count, not a reading of the session clock: a tempo
    /// commit changes what the *next* block adds, and a pause moves the
    /// session's frame axis without moving this. Deriving the position from a
    /// frame pin instead would reinterpret frames already rendered.
    elapsed_beats: f64,
    /// Session beats per frame used at the last planned frontier.
    old_beats_per_frame: Option<f64>,
    /// Interleaved output accumulated across the blocks of one call.
    scratch: Vec<f32>,
    streaming_active: bool,
    streaming_pitch: f64,
    streaming_plan: Option<Arc<RegionPlan>>,
    streaming_region: Option<ActiveRegion>,
    streaming_source_frames: u64,
    streaming_stretch: f64,
    last_input_meta: Option<PcmMeta>,
    pool: PcmPool,
    spec: PcmSpec,
}

impl<E: ElasticPriming> BoundRenderer<E> {
    /// Output frames planned per block. A commit may split the block into two
    /// engine calls inside one exact-span plan.
    pub(crate) const BLOCK_FRAMES: u64 = 512;
    pub(crate) fn resident(
        engine: E,
        span_config: ElasticSpanConfig,
        spec: PcmSpec,
        pool: PcmPool,
    ) -> Self {
        let capabilities = engine.capabilities();
        Self {
            capabilities,
            engine,
            pool,
            schedule: None,
            session_origin: None,
            binding: None,
            span_config,
            spec,
            consumed: 0,
            cursor: None,
            last_input_meta: None,
            old_beats_per_frame: None,
            output_frame: 0,
            priming: priming::State::new(capabilities.latency(), spec),
            elapsed_beats: 0.0,
            pending: Vec::new(),
            pending_start: 0,
            scratch: Vec::new(),
            streaming_active: false,
            streaming_pitch: f64::NAN,
            streaming_plan: None,
            streaming_region: None,
            streaming_source_frames: 0,
            streaming_stretch: f64::NAN,
        }
    }

    pub(crate) fn bind_resident(&mut self, binding: Arc<TempoBinding>) -> Result<(), BoundError> {
        self.priming.activate_resident();
        self.install_binding(binding)
    }

    fn install_binding(&mut self, binding: Arc<TempoBinding>) -> Result<(), BoundError> {
        self.schedule = Some(Arc::clone(&binding.schedule));
        self.session_origin = Some(binding.session_origin);
        self.binding = Some(binding);
        self.pending.clear();
        self.scratch.clear();
        self.cursor = None;
        self.consumed = 0;
        self.output_frame = 0;
        self.elapsed_beats = 0.0;
        self.pending_start = 0;
        self.last_input_meta = None;
        self.streaming_active = false;
        self.streaming_pitch = f64::NAN;
        self.streaming_region = None;
        self.streaming_source_frames = 0;
        self.streaming_stretch = f64::NAN;
        self.old_beats_per_frame = Some(
            self.schedule
                .as_ref()
                .ok_or(BoundError::Inactive)?
                .beats_per_frame()?,
        );
        Ok(())
    }

    pub(crate) fn deactivate(&mut self) {
        self.schedule = None;
        self.session_origin = None;
        self.binding = None;
        self.pending.clear();
        self.scratch.clear();
        self.cursor = None;
        self.consumed = 0;
        self.output_frame = 0;
        self.priming.deactivate();
        self.elapsed_beats = 0.0;
        self.old_beats_per_frame = None;
        self.pending_start = 0;
        self.last_input_meta = None;
        self.streaming_active = false;
        self.streaming_pitch = f64::NAN;
        self.streaming_region = None;
        self.streaming_source_frames = 0;
        self.streaming_stretch = f64::NAN;
    }

    fn admit(&mut self, chunk: &PcmChunk) {
        if chunk.spec() != self.spec {
            self.spec = chunk.spec();
        }
        if self.pending.is_empty() {
            self.pending_start = chunk.meta.frame_offset;
        }
        self.last_input_meta = Some(chunk.meta);
        self.pending.extend_from_slice(&chunk.samples);
    }

    fn channels(&self) -> usize {
        usize::from(self.spec.channels.max(1))
    }

    fn pending_frames(&self) -> u64 {
        (self.pending.len() / self.channels())
            .to_u64()
            .unwrap_or(u64::MAX)
    }

    fn span(&self, start: f64, end: f64, output_frames: usize) -> Result<ElasticSpan, BoundError> {
        let schedule = self.schedule.as_ref().ok_or(BoundError::Inactive)?;
        Ok(ElasticSpan::try_from((
            f64::from(schedule.source_after(start)?)..f64::from(schedule.source_after(end)?),
            output_frames,
        ))?)
    }

    /// Quantized plan for the block starting at the current presentation frame.
    fn plan_block(&self) -> Result<(ElasticSpanPlan, f64, f64), BoundError> {
        let block = usize::try_from(Self::BLOCK_FRAMES).map_err(|_| BoundError::BlockOverflow)?;
        let block_frames = Self::BLOCK_FRAMES
            .to_f64()
            .ok_or(BoundError::BlockOverflow)?;
        let schedule = self.schedule.as_ref().ok_or(BoundError::Inactive)?;
        let session_origin = self.session_origin.ok_or(BoundError::Inactive)?;
        let commit = schedule.commit(session_origin)?;
        let old_beats_per_frame = self
            .old_beats_per_frame
            .unwrap_or_else(|| commit.beats_per_frame());
        let old_next = self.elapsed_beats + old_beats_per_frame * block_frames;
        let commit_frame = if commit.elapsed_beats() > self.elapsed_beats {
            Some(
                ((commit.elapsed_beats() - self.elapsed_beats) / old_beats_per_frame)
                    .round()
                    .to_usize()
                    .ok_or(BoundError::BlockOverflow)?,
            )
        } else {
            None
        };
        let mut spans = [None, None];
        let (next, planned_beats_per_frame) =
            if commit.elapsed_beats() > self.elapsed_beats && commit.elapsed_beats() < old_next {
                let before = commit_frame.ok_or(BoundError::BlockOverflow)?;
                let after = block.checked_sub(before).ok_or(BoundError::BlockOverflow)?;
                let split = commit.elapsed_beats();
                if before == 0 {
                    let next = split + commit.beats_per_frame() * block_frames;
                    spans[0] = Some(self.span(split, next, block)?);
                    (next, commit.beats_per_frame())
                } else if after == 0 {
                    spans[0] = Some(self.span(self.elapsed_beats, split, block)?);
                    (split, commit.beats_per_frame())
                } else {
                    let after_advance = commit.beats_per_frame()
                        * after.to_f64().ok_or(BoundError::BlockOverflow)?;
                    let next = split + after_advance;
                    spans[0] = Some(self.span(self.elapsed_beats, split, before)?);
                    spans[1] = Some(self.span(split, next, after)?);
                    (next, commit.beats_per_frame())
                }
            } else {
                let beats_per_frame = if commit.elapsed_beats() <= self.elapsed_beats {
                    commit.beats_per_frame()
                } else {
                    old_beats_per_frame
                };
                let next = self.elapsed_beats + beats_per_frame * block_frames;
                spans[0] = Some(self.span(self.elapsed_beats, next, block)?);
                let frontier_beats_per_frame = if commit_frame == Some(block) {
                    commit.beats_per_frame()
                } else {
                    beats_per_frame
                };
                (next, frontier_beats_per_frame)
            };
        let plan = ElasticSpanPlan::new(
            spans.into_iter().flatten(),
            self.cursor,
            self.capabilities,
            self.span_config,
        )?;
        Ok((plan, next, planned_beats_per_frame))
    }

    /// Renders every block the pending source can cover, appending each to
    /// `scratch`. Stops without consuming anything when the next block's source
    /// has not arrived.
    pub(super) fn render_available(&mut self) -> Result<(), BoundError> {
        loop {
            let (plan, next_elapsed, planned_beats_per_frame) = self.plan_block()?;
            let segment = *plan.segments().first().ok_or(BoundError::EmptyPlan)?;
            let start =
                u64::try_from(segment.source_start()).map_err(|_| BoundError::BehindWindow {
                    requested: segment.source_start(),
                    available: self.pending_start,
                })?;
            if start < self.pending_start {
                return Err(BoundError::BehindWindow {
                    requested: segment.source_start(),
                    available: self.pending_start,
                });
            }
            let skip = usize::try_from(start - self.pending_start)
                .map_err(|_| BoundError::BlockOverflow)?;
            let source_frames = plan.segments().iter().try_fold(0usize, |total, segment| {
                total
                    .checked_add(segment.request().source_frames())
                    .ok_or(BoundError::BlockOverflow)
            })?;
            let needed = skip
                .checked_add(source_frames)
                .ok_or(BoundError::BlockOverflow)?;
            if self.pending_frames() < needed.to_u64().unwrap_or(u64::MAX) {
                return Ok(());
            }

            let channels = self.channels();
            self.prime_if_needed(start, segment.request())?;

            let mut source_offset = skip;
            for segment in plan.segments() {
                let request = segment.request();
                let source_end = source_offset
                    .checked_add(request.source_frames())
                    .ok_or(BoundError::BlockOverflow)?;
                let written = self.scratch.len();
                let output_samples = request
                    .output_frames()
                    .checked_mul(channels)
                    .ok_or(BoundError::BlockOverflow)?;
                self.scratch.resize(written + output_samples, 0.0);
                let source = &self.pending[source_offset * channels..source_end * channels];
                ElasticEngine::process(
                    &mut self.engine,
                    request,
                    source,
                    &mut self.scratch[written..],
                )?;
                source_offset = source_end;
            }

            let consumed = source_frames.to_u64().unwrap_or(u64::MAX);
            if let Some(binding) = &self.binding {
                let output = plan
                    .segments()
                    .iter()
                    .map(|segment| segment.request().output_frames())
                    .sum::<usize>();
                if output > 0 {
                    binding.set_rate(
                        source_frames.to_f64().unwrap_or_default() / output.to_f64().unwrap_or(1.0),
                    );
                }
            }
            let consumed_end = start.saturating_add(consumed);
            let retained = self
                .retained_source_frames(
                    plan.segments()
                        .last()
                        .ok_or(BoundError::EmptyPlan)?
                        .request(),
                )?
                .to_u64()
                .ok_or(BoundError::BlockOverflow)?;
            let retained_start = consumed_end.saturating_sub(retained);
            let drain_frames = retained_start
                .saturating_sub(self.pending_start)
                .to_usize()
                .ok_or(BoundError::BlockOverflow)?;
            let drain_samples = drain_frames
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            self.pending.drain(..drain_samples);
            self.pending_start = self
                .pending_start
                .saturating_add(drain_frames.to_u64().ok_or(BoundError::BlockOverflow)?);
            self.consumed = self.consumed.saturating_add(consumed);
            self.elapsed_beats = next_elapsed;
            self.old_beats_per_frame = Some(planned_beats_per_frame);
            self.cursor = Some(plan.cursor());
        }
    }

    fn emit(&mut self) -> Option<PcmChunk> {
        if self.scratch.is_empty() {
            return None;
        }
        let mut meta = self.last_input_meta.unwrap_or_default();
        meta.spec = self.spec;
        meta.frames = u32::try_from(self.scratch.len() / self.channels()).unwrap_or(u32::MAX);
        let mut pcm = self.pool.get();
        if pcm.ensure_len(self.scratch.len()).is_err() {
            warn!("PCM pool budget exhausted during bound rendering");
            return None;
        }
        pcm[..].copy_from_slice(&self.scratch);
        let emitted = (self.scratch.len() / self.channels())
            .to_u64()
            .unwrap_or(u64::MAX);
        self.output_frame = self.output_frame.saturating_add(emitted);
        Some(PcmChunk::new(meta, pcm))
    }
}

#[cfg(test)]
impl<E: ElasticPriming> BoundRenderer<E> {
    pub(super) const fn presentation_frame(&self) -> u64 {
        self.output_frame
    }

    pub(super) const fn consumed_source_frames(&self) -> u64 {
        self.consumed
    }

    pub(super) const fn elapsed_session_beats(&self) -> f64 {
        self.elapsed_beats
    }
}

impl<E: ElasticPriming + Send + 'static> AudioEffect for BoundRenderer<E> {
    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        self.pending_frames()
    }

    fn process(&mut self, chunk: PcmChunk) -> DecodeResult<Option<PcmChunk>> {
        self.admit(&chunk);
        self.scratch.clear();
        self.render_available()
            .map_err(|error| DecodeError::pcm_stream("bound tempo renderer", error))?;
        Ok(self.emit())
    }

    fn reset(&mut self) {
        if let Err(error) = ElasticEngine::reset(&mut self.engine) {
            warn!(%error, "bound engine reset failed");
        }
        self.pending.clear();
        self.scratch.clear();
        self.cursor = None;
        self.consumed = 0;
        self.output_frame = 0;
        self.priming.reset();
        self.elapsed_beats = 0.0;
        self.old_beats_per_frame = None;
        self.pending_start = 0;
        self.last_input_meta = None;
        self.streaming_active = false;
        self.streaming_pitch = f64::NAN;
        self.streaming_region = None;
        self.streaming_source_frames = 0;
        self.streaming_stretch = f64::NAN;
    }
}
