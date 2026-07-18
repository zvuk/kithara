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
    player::node::StreamShape,
    session::render::RenderContext,
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
    #[error("another elastic relocation is still pending")]
    RelocationPending,
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
    #[error("elastic relocation was not ready at the committed session boundary")]
    RelocationNotReady,
    #[error("elastic relocation could not be primed at the committed session boundary")]
    RelocationPreparationFailed,
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

struct ElasticRelocation {
    preparation: ElasticPreparation,
    revision: u64,
    samples: Option<PcmBuf>,
    target: SessionBeat,
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
    pending_relocation_request: Option<ElasticSourceRequest>,
    pending_retirement: Option<PcmBuf>,
    preparation: Option<ElasticPreparation>,
    primed: bool,
    relocation: Option<ElasticRelocation>,
    relocation_port: Option<ElasticSourcePort>,
    source_generation: u64,
    source_port: Option<ElasticSourcePort>,
    source_window: Option<SourceFrameRange>,
    fetch: PcmBuf,
    history: PcmBuf,
    source: PcmBuf,
    output: PcmBuf,
    discarded: PcmBuf,
}

impl ElasticRenderer {
    const PREFETCH_BLOCKS: usize = 8;
    const RELOCATION_PREFETCH_BLOCKS: usize = 1;
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
            pending_relocation_request: None,
            pending_retirement: None,
            preparation: None,
            primed: false,
            relocation: None,
            relocation_port: None,
            source_generation: 0,
            source_port: None,
            source_window: None,
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
        if let Some(port) = self.relocation_port.as_mut() {
            port.set_service_class(class);
        }
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
