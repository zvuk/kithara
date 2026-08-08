use kithara_bufpool::PcmPool;
use kithara_decode::{PcmChunk, PcmMeta, PcmSpec};
use kithara_platform::sync::Arc;
use kithara_stretch::{
    ElasticCapabilities, ElasticCursor, ElasticEngine, ElasticSpan, ElasticSpanConfig,
    ElasticSpanPlan,
};
use num_traits::ToPrimitive;
use tracing::warn;

use super::BoundError;
use crate::{musical::SourceSchedule, traits::AudioEffect};

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
/// The slot is forward-only. It never asks for source behind what it has
/// already consumed, so it needs no retention behind the window it reads.
pub(crate) struct BoundRenderer<E> {
    schedule: Arc<SourceSchedule>,
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
    /// Session beats this deck has advanced since its start.
    ///
    /// The deck's own count, not a reading of the session clock: a tempo
    /// commit changes what the *next* block adds, and a pause moves the
    /// session's frame axis without moving this. Deriving the position from a
    /// frame pin instead would reinterpret frames already rendered.
    elapsed_beats: f64,
    /// Interleaved output accumulated across the blocks of one call.
    scratch: Vec<f32>,
    last_input_meta: Option<PcmMeta>,
    pool: PcmPool,
    spec: PcmSpec,
}

impl<E: ElasticEngine> BoundRenderer<E> {
    /// Output frames planned per block. One block is one engine call, so it is
    /// the granularity at which the plan applies its phase correction.
    pub(crate) const BLOCK_FRAMES: u64 = 512;

    pub(crate) fn new(
        schedule: Arc<SourceSchedule>,
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
            schedule,
            span_config,
            spec,
            consumed: 0,
            cursor: None,
            last_input_meta: None,
            output_frame: 0,
            elapsed_beats: 0.0,
            pending: Vec::new(),
            pending_start: 0,
            scratch: Vec::new(),
        }
    }

    fn channels(&self) -> usize {
        usize::from(self.spec.channels.max(1))
    }

    fn pending_frames(&self) -> u64 {
        (self.pending.len() / self.channels())
            .to_u64()
            .unwrap_or(u64::MAX)
    }

    /// Quantized plan for the block starting at the current output frame.
    fn plan_block(&self) -> Result<(ElasticSpanPlan, f64), BoundError> {
        let block = usize::try_from(Self::BLOCK_FRAMES).map_err(|_| BoundError::BlockOverflow)?;
        let per_frame = self.schedule.beats_per_frame()?;
        let advance = per_frame * Self::BLOCK_FRAMES.to_f64().unwrap_or(f64::INFINITY);
        let next = self.elapsed_beats + advance;
        let start = f64::from(self.schedule.source_after(self.elapsed_beats)?);
        let end = f64::from(self.schedule.source_after(next)?);
        let span = ElasticSpan::try_from((start..end, block))?;
        Ok((
            ElasticSpanPlan::new([span], self.cursor, self.capabilities, self.span_config)?,
            next,
        ))
    }

    /// Renders every block the pending source can cover, appending each to
    /// `scratch`. Stops without consuming anything when the next block's source
    /// has not arrived.
    pub(super) fn render_available(&mut self) -> Result<(), BoundError> {
        loop {
            let (plan, next_elapsed) = self.plan_block()?;
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
            let source_frames = segment.request().source_frames();
            let needed = skip
                .checked_add(source_frames)
                .ok_or(BoundError::BlockOverflow)?;
            if self.pending_frames() < needed.to_u64().unwrap_or(u64::MAX) {
                return Ok(());
            }

            let channels = self.channels();
            let written = self.scratch.len();
            let output_samples = segment
                .request()
                .output_frames()
                .checked_mul(channels)
                .ok_or(BoundError::BlockOverflow)?;
            self.scratch.resize(written + output_samples, 0.0);
            let source = &self.pending[skip * channels..needed * channels];
            self.engine
                .process(segment.request(), source, &mut self.scratch[written..])?;

            self.pending.drain(..needed * channels);
            let consumed = source_frames.to_u64().unwrap_or(u64::MAX);
            self.pending_start = start.saturating_add(consumed);
            self.consumed = self.consumed.saturating_add(consumed);
            self.output_frame = self.output_frame.saturating_add(Self::BLOCK_FRAMES);
            self.elapsed_beats = next_elapsed;
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
        Some(PcmChunk::new(meta, pcm))
    }
}

impl<E: ElasticEngine + Send + 'static> AudioEffect for BoundRenderer<E> {
    fn flush(&mut self) -> Option<PcmChunk> {
        None
    }

    fn held_source_frames(&self) -> u64 {
        let latency = self
            .capabilities
            .latency()
            .source_frames()
            .to_u64()
            .unwrap_or(u64::MAX);
        self.pending_frames()
            .saturating_add(latency.min(self.consumed))
    }

    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk> {
        if chunk.spec() != self.spec {
            self.spec = chunk.spec();
        }
        if self.pending.is_empty() {
            self.pending_start = chunk.meta.frame_offset;
        }
        self.last_input_meta = Some(chunk.meta);
        self.pending.extend_from_slice(&chunk.samples);
        self.scratch.clear();
        if let Err(error) = self.render_available() {
            warn!(%error, "bound rendering failed; dropping block");
            return None;
        }
        self.emit()
    }

    fn reset(&mut self) {
        if let Err(error) = self.engine.reset() {
            warn!(%error, "bound engine reset failed");
        }
        self.pending.clear();
        self.scratch.clear();
        self.cursor = None;
        self.consumed = 0;
        self.output_frame = 0;
        self.elapsed_beats = 0.0;
        self.pending_start = 0;
        self.last_input_meta = None;
    }
}
