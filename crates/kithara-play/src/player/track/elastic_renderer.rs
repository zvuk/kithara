#[path = "preparation.rs"]
mod preparation;
#[path = "rendering.rs"]
mod rendering;

use std::{mem::replace, num::NonZeroU32};

use kithara_audio::{
    SourceFrameIndex, SourceRange, SourceRangeError, SourceRangeReadOutcome, SourceRangeRequest,
};
use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use kithara_stretch::{ElasticConfig, ElasticError, ElasticRequest, SignalsmithBackend};
use num_traits::ToPrimitive;
use rendering::SourceCopy;
use smallvec::SmallVec;

use super::elastic::{ElasticPlanError, ElasticRenderSegment, plan_elastic_segments};
use crate::{
    api::{
        PlaybackDirection, SessionBeat, SyncUnavailable, Tempo, TrackBinding, TransportRevision,
    },
    player::node::StreamShape,
    resource::Resource,
    session::render::RenderContext,
};

const READY_WINDOW_COUNT: usize = 2;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ElasticPrepareError {
    #[error("bound playback requires canonical decoded source ranges")]
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
    Source(#[from] SourceRangeError),
    #[error(transparent)]
    Backend(#[from] ElasticError),
    #[error("decoded source ended before elastic preparation completed")]
    SourceEnded,
    #[error("the elastic backend does not support reverse input")]
    ReverseUnsupported,
    #[error("elastic preparation source range is outside the prepared fetch window")]
    FetchWindowMismatch,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ElasticRenderError {
    #[error(transparent)]
    Plan(#[from] ElasticPlanError),
    #[error(transparent)]
    Source(#[from] SourceRangeError),
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
    #[error("elastic renderer history was prepared for a different playback direction")]
    DirectionMismatch,
    #[error("elastic source reader is unavailable")]
    SourceUnavailable,
    #[error("elastic source read failed")]
    SourceReadFailed,
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

#[derive(Clone, Copy, Debug)]
struct IntegerSegment {
    source_end: i64,
    source_start: i64,
    output_frames: usize,
    output_start: usize,
}

#[derive(Clone, Copy, Debug)]
struct SourceCursor {
    continuous: f64,
    integer: i64,
}

#[derive(Clone, Copy)]
struct ElasticPreparation {
    warmup: ElasticRequest,
    direction: PlaybackDirection,
    anchor: SourceCursor,
    fetch_range: SourceRange,
}

#[derive(Clone, Copy)]
struct PreparedRuntime {
    cursor: SourceCursor,
    direction: PlaybackDirection,
    source_window: SourceRange,
    request_id: u64,
    revision: TransportRevision,
}

struct RenderRuntime {
    prepared: PreparedRuntime,
    pending_source_read: Option<PendingSourceRead>,
}

#[derive(Clone, Copy)]
enum PreparationPhase {
    Priming {
        request: SourceRangeRequest,
        range: SourceRange,
        extension: Option<SourceRange>,
    },
    Window {
        request: SourceRangeRequest,
        range: SourceRange,
        runtime: PreparedRuntime,
    },
}

pub(crate) struct Preparing {
    preparation: ElasticPreparation,
    phase: PreparationPhase,
    revision: TransportRevision,
}

pub(crate) struct Ready {
    runtime: RenderRuntime,
}

pub(crate) struct Active {
    runtime: RenderRuntime,
}

pub(crate) enum ElasticPreparationPoll {
    Pending(ElasticRenderer<Preparing>),
    Ready(ElasticRenderer<Ready>),
}

pub(crate) type PreparingElasticRenderer = ElasticRenderer<Preparing>;

struct BufferedSourceWindow {
    samples: PcmBuf,
    range: SourceRange,
}

struct PendingSourceRead {
    samples: PcmBuf,
    request: SourceRangeRequest,
}

pub(crate) struct ElasticRenderer<State> {
    state: State,
    sample_rate: NonZeroU32,
    discarded: PcmBuf,
    fetch: PcmBuf,
    history: PcmBuf,
    output: PcmBuf,
    source: PcmBuf,
    backend: SignalsmithBackend<ElasticConfig>,
    ready_windows: SmallVec<[BufferedSourceWindow; READY_WINDOW_COUNT]>,
    window_buffers: SmallVec<[PcmBuf; READY_WINDOW_COUNT]>,
    source_frame_count: u64,
    source_window_frames: u64,
    max_fetch_frames: usize,
    max_source_frames: usize,
    max_warm_frames: usize,
}

impl<State> ElasticRenderer<State> {
    const PREFETCH_BLOCKS: usize = 8;
    const SOURCE_CAPACITY_MULTIPLIER: usize = 2;
    const SOURCE_WINDOW_BLOCKS: usize = 8;
    const SUPPORTED_CHANNEL_COUNT: std::ops::RangeInclusive<usize> = 1..=2;

    fn map_state<Next>(self, map: impl FnOnce(State) -> Next) -> ElasticRenderer<Next> {
        let Self {
            state,
            sample_rate,
            discarded,
            fetch,
            history,
            output,
            source,
            backend,
            ready_windows,
            window_buffers,
            source_frame_count,
            source_window_frames,
            max_fetch_frames,
            max_source_frames,
            max_warm_frames,
        } = self;
        ElasticRenderer {
            state: map(state),
            sample_rate,
            discarded,
            fetch,
            history,
            output,
            source,
            backend,
            ready_windows,
            window_buffers,
            source_frame_count,
            source_window_frames,
            max_fetch_frames,
            max_source_frames,
            max_warm_frames,
        }
    }

    pub(super) fn next_source_window(
        &self,
        current: SourceRange,
        direction: PlaybackDirection,
    ) -> Result<Option<SourceRange>, SourceRangeError> {
        let overlap = SourceFrameIndex::try_from(self.max_source_frames)?.get();
        let (start, end) = match direction {
            PlaybackDirection::Forward => {
                let start = current.end().get().saturating_sub(overlap);
                let end = start
                    .saturating_add(self.source_window_frames)
                    .min(self.source_frame_count);
                (start, end)
            }
            PlaybackDirection::Reverse => {
                let end = current
                    .start()
                    .get()
                    .saturating_add(overlap)
                    .min(self.source_frame_count);
                let start = end.saturating_sub(self.source_window_frames);
                (start, end)
            }
        };
        let advances = match direction {
            PlaybackDirection::Forward => end > current.end().get(),
            PlaybackDirection::Reverse => start < current.start().get(),
        };
        if !advances || start >= end {
            return Ok(None);
        }
        SourceRange::try_from(start..end).map(Some)
    }
}

impl ElasticRenderer<Active> {
    pub(super) fn decoded_frontier(&self) -> f64 {
        std::iter::once(self.state.runtime.prepared.source_window.end().get())
            .chain(
                self.ready_windows
                    .iter()
                    .map(|window| window.range.end().get()),
            )
            .max()
            .and_then(|frame| frame.to_f64())
            .map_or(0.0, |frame| frame / f64::from(self.sample_rate.get()))
    }

    pub(super) fn ensure_window(
        &mut self,
        source: &mut Resource,
        range: SourceRange,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        self.poll_source_read(source, direction)?;
        let source_window = self.state.runtime.prepared.source_window;
        if source_window.start() <= range.start() && range.end() <= source_window.end() {
            self.schedule_window(source, direction)?;
            return Ok(());
        }
        if self
            .ready_windows
            .first()
            .map(|window| window.range)
            .is_some_and(|window| window.start() <= range.start() && range.end() <= window.end())
        {
            let window = self.ready_windows.remove(0);
            let old = replace(&mut self.fetch, window.samples);
            self.window_buffers.push(old);
            self.state.runtime.prepared.source_window = window.range;
            self.schedule_window(source, direction)?;
            return Ok(());
        }
        self.schedule_window(source, direction)?;
        Err(ElasticRenderError::SourceWindowDeadlineMissed)
    }

    pub(super) fn poll_source_read(
        &mut self,
        source: &mut Resource,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        let Some(pending) = self.state.runtime.pending_source_read.as_mut() else {
            return Ok(());
        };
        let range = pending.request.range();
        let frames = usize::try_from(range.len()).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let sample_len = frames
            .checked_mul(self.backend.capabilities().channels())
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let outcome = source.read_source_range(pending.request, &mut pending.samples[..sample_len]);
        match outcome {
            Ok(SourceRangeReadOutcome::Pending) => Ok(()),
            Ok(SourceRangeReadOutcome::Ready { .. }) => {
                let pending = self
                    .state
                    .runtime
                    .pending_source_read
                    .take()
                    .ok_or(ElasticRenderError::SourceUnavailable)?;
                self.stage_ready_window(pending.request.range(), pending.samples, direction)
            }
            Ok(SourceRangeReadOutcome::Eof) => {
                self.release_pending_source_read();
                Err(ElasticRenderError::SourceReadFailed)
            }
            Err(error) => {
                self.release_pending_source_read();
                Err(error.into())
            }
        }
    }
}
impl ElasticRenderer<()> {
    fn allocate(
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
            .checked_mul(Self::SOURCE_CAPACITY_MULTIPLIER)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let config = ElasticConfig::new(
            shape.sample_rate.get(),
            channels,
            prepared_source_limit,
            max_output_frames,
        )?;
        let backend = SignalsmithBackend::prepare(config)?;
        let capabilities = backend.capabilities();
        let rate = capabilities.rate_envelope().max_source_frames_per_output();
        if !Self::SUPPORTED_CHANNEL_COUNT.contains(&channels) {
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
        let preparation_frames = latency
            .source_frames()
            .checked_add(max_warm_frames)
            .and_then(|frames| frames.checked_add(prefetch_frames))
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let source_window_frames = max_source_frames
            .checked_mul(Self::SOURCE_WINDOW_BLOCKS)
            .ok_or(ElasticPrepareError::FrameOverflow)?;
        let max_fetch_frames = preparation_frames.max(source_window_frames);
        let source_window_frames =
            u64::try_from(source_window_frames).map_err(|_| ElasticPrepareError::FrameOverflow)?;
        let fetch_samples = sample_count(max_fetch_frames, channels)?;
        let window_buffers = (0..READY_WINDOW_COUNT)
            .map(|_| prepared_buffer(pool, fetch_samples))
            .collect::<Result<SmallVec<_>, _>>()?;

        Ok(Self {
            state: (),
            backend,
            max_warm_frames,
            max_source_frames,
            max_fetch_frames,
            source_frame_count,
            source_window_frames,
            window_buffers,
            sample_rate: shape.sample_rate,
            ready_windows: SmallVec::new(),
            fetch: prepared_buffer(pool, fetch_samples)?,
            history: prepared_buffer(pool, sample_count(latency.source_frames(), channels)?)?,
            source: prepared_buffer(pool, sample_count(source_buffer_frames, channels)?)?,
            output: prepared_buffer(pool, sample_count(max_output_frames, channels)?)?,
            discarded: prepared_buffer(pool, sample_count(latency.output_frames(), channels)?)?,
        })
    }
}
impl ElasticRenderer<Active> {
    fn release_pending_source_read(&mut self) {
        if let Some(pending) = self.state.runtime.pending_source_read.take() {
            self.window_buffers.push(pending.samples);
        }
    }

    pub(super) fn schedule_window(
        &mut self,
        source: &mut Resource,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        if self.ready_windows.len() >= READY_WINDOW_COUNT {
            return Ok(());
        }
        if self.state.runtime.pending_source_read.is_some() || self.window_buffers.is_empty() {
            return Ok(());
        }
        let current = self
            .ready_windows
            .last()
            .map_or(self.state.runtime.prepared.source_window, |window| {
                window.range
            });
        let Some(request_range) = self.next_source_window(current, direction)? else {
            return Ok(());
        };
        let request = source.request_source_range(request_range)?;
        let samples = self
            .window_buffers
            .pop()
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        self.state.runtime.pending_source_read = Some(PendingSourceRead { samples, request });
        Ok(())
    }

    fn stage_ready_window(
        &mut self,
        range: SourceRange,
        samples: PcmBuf,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        let frontier = self
            .ready_windows
            .last()
            .map_or(self.state.runtime.prepared.source_window, |window| {
                window.range
            });
        let advances = match direction {
            PlaybackDirection::Forward => range.end() > frontier.end(),
            PlaybackDirection::Reverse => range.start() < frontier.start(),
        };
        if !advances || self.ready_windows.len() >= READY_WINDOW_COUNT {
            self.window_buffers.push(samples);
            return Err(ElasticRenderError::SourceReadFailed);
        }
        self.ready_windows
            .push(BufferedSourceWindow { samples, range });
        Ok(())
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
