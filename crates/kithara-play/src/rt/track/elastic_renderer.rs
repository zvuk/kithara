#[path = "preparation.rs"]
mod preparation;
#[path = "rendering.rs"]
mod rendering;

use std::num::NonZeroU32;

use kithara_audio::{
    ServiceClass, SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange,
};
use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use kithara_stretch::{
    ElasticBackend, ElasticCapabilities, ElasticConfig, ElasticError, ElasticRequest,
    SignalsmithElastic,
};
use num_traits::ToPrimitive;
use rendering::{SourceCopy, copy_source};

use super::{
    PlayerResource,
    elastic::{ElasticPlanError, ElasticRenderSegment, plan_elastic_segments},
    elastic_source::{
        ElasticSourcePort, ElasticSourceReply, ElasticSourceRequest, ElasticSourceWindow,
    },
};
use crate::{
    api::{PlaybackDirection, SessionBeat, SyncUnavailable, Tempo, TrackBinding},
    rt::{StreamShape, context::RenderContext},
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ElasticPrepareError {
    #[error("bound playback requires a decoded source-audio lane")]
    SourceUnavailable,
    #[error("track binding could not resolve its source anchor: {0}")]
    Binding(#[from] SyncUnavailable),
    #[error("track binding anchor lies outside the analysed marker domain")]
    AnchorOutsideMarkerDomain,
    #[error("elastic renderer source format does not match the session stream")]
    FormatMismatch,
    #[error("elastic rendering supports only mono and stereo sources")]
    UnsupportedChannelLayout,
    #[error("elastic renderer frame arithmetic overflowed")]
    FrameOverflow,
    #[error("elastic renderer buffer budget is exhausted")]
    BufferBudget(#[from] BudgetExhausted),
    #[error(transparent)]
    Source(#[from] SourceAudioError),
    #[error(transparent)]
    Backend(#[from] ElasticError),
    #[error("source seek failed while preparing elastic playback")]
    SourceSeek,
    #[error("decoded source ended before elastic preparation completed")]
    SourceEnded,
    #[error("reverse playback requires separately prepared directional state")]
    ReversePreparationRequired,
    #[error("source-audio reader returned an unsupported preparation outcome")]
    UnsupportedSourceOutcome,
    #[error("elastic preparation source range is outside the prepared fetch window")]
    FetchWindowMismatch,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ElasticRenderError {
    #[error(transparent)]
    Plan(#[from] ElasticPlanError),
    #[error(transparent)]
    Source(#[from] SourceAudioError),
    #[error(transparent)]
    Backend(#[from] ElasticError),
    #[error("elastic source frame arithmetic overflowed")]
    FrameOverflow,
    #[error("elastic source cursor is discontinuous: expected {expected}, received {actual}")]
    DiscontinuousSource { expected: f64, actual: f64 },
    #[error(
        "elastic phase error {error} exceeds the continuous correction limit {limit}; prepared relocation is required"
    )]
    RelocationRequired { error: f64, limit: f64 },
    #[error("elastic phase error {error} cannot be corrected inside the backend rate envelope")]
    PhaseCorrectionUnavailable { error: f64 },
    #[error("elastic render request revision does not match the prepared stream")]
    RevisionMismatch,
    #[error("elastic render context has no committed session transport")]
    TransportCommitUnavailable,
    #[error("elastic source range is outside the prepared fetch window")]
    FetchWindowMismatch,
    #[error("elastic renderer output channel layout does not match its preparation")]
    OutputChannelMismatch,
    #[error("elastic renderer was not prepared for this resource")]
    NotPrepared,
    #[error("reverse playback requires separately prepared directional state")]
    ReversePreparationRequired,
    #[error("elastic source preparation worker is unavailable")]
    SourceWorkerUnavailable,
    #[error("elastic source preparation worker failed")]
    SourceWorkerFailed,
    #[error("elastic source window missed its render deadline")]
    SourceWindowDeadlineMissed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElasticCopyError {
    FrameOverflow,
    FetchWindowMismatch,
}

impl From<ElasticCopyError> for ElasticPrepareError {
    fn from(error: ElasticCopyError) -> Self {
        match error {
            ElasticCopyError::FrameOverflow => Self::FrameOverflow,
            ElasticCopyError::FetchWindowMismatch => Self::FetchWindowMismatch,
        }
    }
}

impl From<ElasticCopyError> for ElasticRenderError {
    fn from(error: ElasticCopyError) -> Self {
        match error {
            ElasticCopyError::FrameOverflow => Self::FrameOverflow,
            ElasticCopyError::FetchWindowMismatch => Self::FetchWindowMismatch,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ElasticRenderOutcome {
    Ready { frames: usize },
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ElasticPreparationOutcome {
    Ready,
    Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ElasticTempoPreparationOutcome {
    Pending,
    Ready { raw_latency_frames: u32 },
}

#[derive(Clone, Copy, Debug)]
struct IntegerSegment {
    output_start: usize,
    output_frames: usize,
    source_start: i64,
    source_end: i64,
}

#[derive(Clone, Copy, Debug)]
struct SourceCursor {
    continuous: f64,
    integer: i64,
}

#[derive(Clone, Copy)]
struct ElasticPreparation {
    anchor: SourceCursor,
    fetch_range: SourceFrameRange,
    warmup: ElasticRequest,
}

#[derive(Clone, Copy)]
struct TempoSourcePin {
    range: SourceFrameRange,
    revision: u64,
}

pub(crate) struct ElasticRenderer {
    backend: Box<dyn ElasticBackend>,
    capabilities: ElasticCapabilities,
    channels: usize,
    max_source_frames: usize,
    max_fetch_frames: usize,
    sample_rate: NonZeroU32,
    source_frame_count: u64,
    request_id: u64,
    revision: u64,
    demand: Option<SourceAudioDemand>,
    cursor: Option<SourceCursor>,
    pending_request: Option<ElasticSourceRequest>,
    pending_retirement: Option<PcmBuf>,
    preparation: Option<ElasticPreparation>,
    primed: bool,
    source_generation: u64,
    source_port: Option<ElasticSourcePort>,
    source_window: Option<SourceFrameRange>,
    tempo_pin: Option<TempoSourcePin>,
    fetch: PcmBuf,
    history: PcmBuf,
    source: PcmBuf,
    output: PcmBuf,
    discarded: PcmBuf,
}

impl ElasticRenderer {
    const PREFETCH_BLOCKS: usize = 8;
    const RENEWAL_BLOCKS: usize = 2;

    pub(crate) fn prepare(
        spec_sample_rate: NonZeroU32,
        channels: usize,
        source_frame_count: u64,
        shape: StreamShape,
        pool: &PcmPool,
    ) -> Result<Self, ElasticPrepareError> {
        if spec_sample_rate != shape.sample_rate {
            return Err(ElasticPrepareError::FormatMismatch);
        }
        let max_output_frames = usize::try_from(shape.max_block_frames.get())
            .map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let prepared_source_limit = max_output_frames
            .checked_mul(2)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let config = ElasticConfig::new(
            shape.sample_rate.get(),
            channels,
            prepared_source_limit,
            max_output_frames,
        )?;
        let backend = SignalsmithElastic::prepare(config)?;
        let capabilities = backend.capabilities();
        let rate = capabilities.rate_envelope().max_source_frames_per_output();
        if !(1..=2).contains(&channels) {
            return Err(ElasticPrepareError::UnsupportedChannelLayout);
        }
        let max_source_frames = scaled_frames(max_output_frames, rate)?
            .checked_add(1)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let latency = capabilities.latency();
        let max_warm_frames = capabilities.warmup_request(rate)?.source_frames();
        let source_buffer_frames = max_source_frames.max(max_warm_frames);
        let prefetch_frames = max_source_frames
            .checked_mul(Self::PREFETCH_BLOCKS)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let max_fetch_frames = latency
            .source_frames()
            .checked_add(max_warm_frames)
            .and_then(|frames| frames.checked_add(prefetch_frames))
            .ok_or(ElasticPrepareError::FrameOverflow)?;

        Ok(Self {
            backend: Box::new(backend),
            capabilities,
            channels,
            max_source_frames,
            max_fetch_frames,
            sample_rate: shape.sample_rate,
            source_frame_count,
            request_id: 1,
            revision: 0,
            demand: None,
            cursor: None,
            pending_request: None,
            pending_retirement: None,
            preparation: None,
            primed: false,
            source_generation: 0,
            source_port: None,
            source_window: None,
            tempo_pin: None,
            fetch: prepared_buffer(pool, sample_count(max_fetch_frames, channels)?)?,
            history: prepared_buffer(pool, sample_count(latency.source_frames(), channels)?)?,
            source: prepared_buffer(pool, sample_count(source_buffer_frames, channels)?)?,
            output: prepared_buffer(pool, sample_count(max_output_frames, channels)?)?,
            discarded: prepared_buffer(pool, sample_count(latency.output_frames(), channels)?)?,
        })
    }

    pub(super) fn decoded_frontier(&self) -> f64 {
        self.source_window
            .and_then(|window| window.end().to_f64())
            .map_or(0.0, |frame| frame / f64::from(self.sample_rate.get()))
    }

    pub(super) fn set_service_class(&mut self, class: ServiceClass) {
        if let Some(port) = self.source_port.as_mut() {
            port.set_service_class(class);
        }
    }

    pub(crate) fn prepare_tempo(
        &mut self,
        binding: &TrackBinding,
        current: &RenderContext,
        candidate: &RenderContext,
        revision: u64,
    ) -> Result<ElasticTempoPreparationOutcome, ElasticRenderError> {
        if !self.primed {
            return Err(ElasticRenderError::NotPrepared);
        }
        if binding.direction() != PlaybackDirection::Forward {
            return Err(ElasticRenderError::ReversePreparationRequired);
        }
        let active = current
            .transport_commit()
            .ok_or(ElasticRenderError::TransportCommitUnavailable)?;
        let current_beat = current
            .session_beats()
            .ok_or(ElasticRenderError::TransportCommitUnavailable)?
            .end;
        let candidate_beats = candidate
            .session_beats()
            .ok_or(ElasticRenderError::TransportCommitUnavailable)?;
        let current_track_beat = binding
            .track_beat_at(current_beat)
            .map_err(ElasticPlanError::Binding)?;
        let current_source = binding
            .map()
            .source_frame_at(current_track_beat)
            .ok_or_else(|| ElasticPlanError::OutsideMarkerDomain {
                beat: current_track_beat.get(),
            })?
            .get();
        let old_output_frames = candidate
            .render_frames()
            .start
            .get()
            .checked_sub(current.render_frames().end.get())
            .and_then(|frames| usize::try_from(frames).ok())
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let old_context = RenderContext::new(
            current.render_frames().end..candidate.render_frames().start,
            current.sample_rate(),
            Some(current_beat..candidate_beats.start),
            Some(active),
        )
        .ok_or(ElasticPlanError::InvalidOutputRange)?;
        let candidate_output_frames = candidate
            .render_frames()
            .end
            .get()
            .checked_sub(candidate.render_frames().start.get())
            .and_then(|frames| usize::try_from(frames).ok())
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let envelope = self.capabilities.rate_envelope();
        let old_planned = plan_elastic_segments(
            binding,
            &old_context,
            0..old_output_frames,
            self.request_id,
            active.revision(),
            envelope.min_source_frames_per_output()..=envelope.max_source_frames_per_output(),
        )?;
        let candidate_planned = plan_elastic_segments(
            binding,
            candidate,
            0..candidate_output_frames,
            self.request_id,
            revision,
            envelope.min_source_frames_per_output()..=envelope.max_source_frames_per_output(),
        )?;
        let mut lower = current_source;
        let mut upper = current_source;
        for segment in old_planned.iter().chain(&candidate_planned) {
            lower = lower
                .min(segment.request.source_start())
                .min(segment.request.source_end());
            upper = upper
                .max(segment.request.source_start())
                .max(segment.request.source_end());
        }
        let start = lower
            .floor()
            .to_i64()
            .ok_or(ElasticRenderError::FrameOverflow)?
            .saturating_sub(1)
            .max(0);
        let end = upper
            .ceil()
            .to_i64()
            .ok_or(ElasticRenderError::FrameOverflow)?
            .checked_add(1)
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let source_end = i64::try_from(self.source_frame_count)
            .map_err(|_| ElasticRenderError::FrameOverflow)?;
        let start = u64::try_from(start).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let end =
            u64::try_from(end.min(source_end)).map_err(|_| ElasticRenderError::FrameOverflow)?;
        if start >= end {
            return Err(ElasticRenderError::FetchWindowMismatch);
        }
        self.prepare_tempo_window(revision, SourceFrameRange::new(start, end)?)
    }

    pub(crate) fn abort_tempo(&mut self, revision: u64) {
        if self.tempo_pin.is_some_and(|pin| pin.revision == revision) {
            self.tempo_pin = None;
        }
    }

    pub(crate) fn apply_tempo(&mut self, revision: u64) {
        self.abort_tempo(revision);
    }

    fn prepare_tempo_window(
        &mut self,
        revision: u64,
        range: SourceFrameRange,
    ) -> Result<ElasticTempoPreparationOutcome, ElasticRenderError> {
        self.poll_source_port()?;
        self.tempo_pin = Some(TempoSourcePin { range, revision });
        let required = self.pinned_range(range)?;
        let resident_covers = self
            .source_window
            .is_some_and(|window| preparation::range_contains(window, required));
        let pending_covers = self
            .pending_request
            .is_none_or(|request| preparation::range_contains(request.range(), required));
        if resident_covers && pending_covers {
            self.schedule_window(required)?;
            let raw_latency_frames = u32::try_from(self.capabilities.latency().output_frames())
                .map_err(|_| ElasticRenderError::FrameOverflow)?;
            return Ok(ElasticTempoPreparationOutcome::Ready { raw_latency_frames });
        }
        if let Err(error) = self.schedule_window(required) {
            self.abort_tempo(revision);
            return Err(error);
        }
        Ok(ElasticTempoPreparationOutcome::Pending)
    }
}

fn prepared_buffer(pool: &PcmPool, samples: usize) -> Result<PcmBuf, BudgetExhausted> {
    let mut buffer = pool.get();
    buffer.ensure_len(samples)?;
    Ok(buffer)
}

fn sample_count(frames: usize, channels: usize) -> Result<usize, ElasticPrepareError> {
    frames
        .checked_mul(channels)
        .ok_or(ElasticPrepareError::FrameOverflow)
}

fn scaled_frames(frames: usize, rate: f64) -> Result<usize, ElasticPrepareError> {
    (frames.to_f64().ok_or(ElasticPrepareError::FrameOverflow)? * rate)
        .ceil()
        .to_usize()
        .ok_or(ElasticPrepareError::FrameOverflow)
}

fn quantize_source(source: f64) -> Result<i64, ElasticRenderError> {
    source
        .round()
        .to_i64()
        .ok_or(ElasticRenderError::FrameOverflow)
}
