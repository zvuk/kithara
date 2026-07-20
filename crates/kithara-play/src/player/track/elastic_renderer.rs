#[path = "preparation.rs"]
mod preparation;
#[path = "rendering.rs"]
mod rendering;

use std::{mem::replace, num::NonZeroU32};

use kithara_audio::{
    SourceAudioDemand, SourceAudioError, SourceAudioReadOutcome, SourceFrameRange,
};
use kithara_bufpool::{BudgetExhausted, PcmBuf, PcmPool};
use kithara_stretch::{
    ElasticCapabilities, ElasticConfig, ElasticError, ElasticRequest, SignalsmithElastic,
};
use num_traits::ToPrimitive;
use rendering::SourceCopy;
use smallvec::SmallVec;

use super::{
    PlayerResource,
    elastic::{ElasticPlanError, ElasticRenderSegment, plan_elastic_segments},
};
use crate::{
    api::{PlaybackDirection, SessionBeat, SyncUnavailable, Tempo, TrackBinding},
    player::node::StreamShape,
    resource::Resource,
    session::render::RenderContext,
};

const READY_WINDOW_COUNT: usize = 2;

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
    #[error("the elastic backend does not support reverse input")]
    ReverseUnsupported,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ElasticPreparationOutcome {
    Ready,
    Pending,
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
    fetch_range: SourceFrameRange,
}

struct BufferedSourceWindow {
    samples: PcmBuf,
    range: SourceFrameRange,
}

struct PendingSourceRead {
    samples: PcmBuf,
    demand: SourceAudioDemand,
    range: SourceFrameRange,
}

pub(crate) struct ElasticRenderer {
    capabilities: ElasticCapabilities,
    sample_rate: NonZeroU32,
    cursor: Option<SourceCursor>,
    demand: Option<SourceAudioDemand>,
    direction: Option<PlaybackDirection>,
    pending_source_read: Option<PendingSourceRead>,
    preparation: Option<ElasticPreparation>,
    preparation_extension: Option<SourceFrameRange>,
    preparation_request: Option<SourceFrameRange>,
    preparation_window: Option<SourceFrameRange>,
    source_window: Option<SourceFrameRange>,
    discarded: PcmBuf,
    fetch: PcmBuf,
    history: PcmBuf,
    output: PcmBuf,
    source: PcmBuf,
    backend: SignalsmithElastic,
    ready_windows: SmallVec<[BufferedSourceWindow; READY_WINDOW_COUNT]>,
    window_buffers: SmallVec<[PcmBuf; READY_WINDOW_COUNT]>,
    primed: bool,
    request_id: u64,
    revision: u64,
    source_frame_count: u64,
    source_window_frames: u64,
    max_fetch_frames: usize,
    max_source_frames: usize,
    max_warm_frames: usize,
}

impl ElasticRenderer {
    const PREFETCH_BLOCKS: usize = 8;
    const SOURCE_WINDOW_BLOCKS: usize = 8;

    pub(super) fn decoded_frontier(&self) -> f64 {
        self.source_window
            .iter()
            .map(|window| window.end())
            .chain(self.ready_windows.iter().map(|window| window.range.end()))
            .max()
            .and_then(|frame| frame.to_f64())
            .map_or(0.0, |frame| frame / f64::from(self.sample_rate.get()))
    }

    pub(super) fn ensure_window(
        &mut self,
        source: &mut Resource,
        range: SourceFrameRange,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        self.poll_source_read(source, direction)?;
        if self
            .source_window
            .is_some_and(|window| window.start() <= range.start() && range.end() <= window.end())
        {
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
            self.source_window = Some(window.range);
            self.schedule_window(source, direction)?;
            return Ok(());
        }
        self.schedule_window(source, direction)?;
        Err(ElasticRenderError::SourceWindowDeadlineMissed)
    }

    pub(super) fn next_source_window(
        &self,
        current: SourceFrameRange,
        direction: PlaybackDirection,
    ) -> Result<Option<SourceFrameRange>, SourceAudioError> {
        let overlap =
            u64::try_from(self.max_source_frames).map_err(|_| SourceAudioError::FrameOverflow)?;
        let (start, end) = match direction {
            PlaybackDirection::Forward => {
                let start = current.end().saturating_sub(overlap);
                let end = start
                    .saturating_add(self.source_window_frames)
                    .min(self.source_frame_count);
                (start, end)
            }
            PlaybackDirection::Reverse => {
                let end = current
                    .start()
                    .saturating_add(overlap)
                    .min(self.source_frame_count);
                let start = end.saturating_sub(self.source_window_frames);
                (start, end)
            }
        };
        let advances = match direction {
            PlaybackDirection::Forward => end > current.end(),
            PlaybackDirection::Reverse => start < current.start(),
        };
        if !advances || start >= end {
            return Ok(None);
        }
        SourceFrameRange::new(start, end).map(Some)
    }

    pub(super) fn poll_source_read(
        &mut self,
        source: &mut Resource,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        let Some(pending) = self.pending_source_read.as_mut() else {
            return Ok(());
        };
        let frames =
            usize::try_from(pending.range.len()).map_err(|_| ElasticRenderError::FrameOverflow)?;
        let sample_len = frames
            .checked_mul(self.capabilities.channels())
            .ok_or(ElasticRenderError::FrameOverflow)?;
        let outcome = source.read_source_audio(
            &pending.demand,
            pending.range,
            &mut pending.samples[..sample_len],
        );
        match outcome {
            Ok(Some(SourceAudioReadOutcome::Pending)) => Ok(()),
            Ok(Some(SourceAudioReadOutcome::Ready { .. })) => {
                let pending = self
                    .pending_source_read
                    .take()
                    .ok_or(ElasticRenderError::SourceUnavailable)?;
                self.stage_ready_window(pending.range, pending.samples, direction)
            }
            Ok(Some(SourceAudioReadOutcome::Eof)) | Ok(None) | Ok(Some(_)) => {
                self.release_pending_source_read();
                Err(ElasticRenderError::SourceReadFailed)
            }
            Err(error) => {
                self.release_pending_source_read();
                Err(error.into())
            }
        }
    }

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
            backend,
            capabilities,
            max_warm_frames,
            max_source_frames,
            max_fetch_frames,
            source_frame_count,
            source_window_frames,
            window_buffers,
            sample_rate: shape.sample_rate,
            request_id: 1,
            revision: 0,
            demand: None,
            cursor: None,
            direction: None,
            pending_source_read: None,
            preparation: None,
            preparation_extension: None,
            preparation_request: None,
            preparation_window: None,
            primed: false,
            source_window: None,
            ready_windows: SmallVec::new(),
            fetch: prepared_buffer(pool, fetch_samples)?,
            history: prepared_buffer(pool, sample_count(latency.source_frames(), channels)?)?,
            source: prepared_buffer(pool, sample_count(source_buffer_frames, channels)?)?,
            output: prepared_buffer(pool, sample_count(max_output_frames, channels)?)?,
            discarded: prepared_buffer(pool, sample_count(latency.output_frames(), channels)?)?,
        })
    }

    fn release_pending_source_read(&mut self) {
        if let Some(pending) = self.pending_source_read.take() {
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
        if self.pending_source_read.is_some() || self.window_buffers.is_empty() {
            return Ok(());
        }
        let current = self
            .ready_windows
            .last()
            .map(|window| window.range)
            .or(self.source_window)
            .ok_or(ElasticRenderError::FetchWindowMismatch)?;
        let Some(request_range) = self.next_source_window(current, direction)? else {
            return Ok(());
        };
        source
            .seek_source_frame(request_range.start())
            .map_err(|_| ElasticRenderError::SourceReadFailed)?;
        let demand = source
            .request_source_audio(request_range, 0)?
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        let samples = self
            .window_buffers
            .pop()
            .ok_or(ElasticRenderError::SourceUnavailable)?;
        self.pending_source_read = Some(PendingSourceRead {
            demand,
            samples,
            range: request_range,
        });
        Ok(())
    }

    fn stage_ready_window(
        &mut self,
        range: SourceFrameRange,
        samples: PcmBuf,
        direction: PlaybackDirection,
    ) -> Result<(), ElasticRenderError> {
        let frontier = self
            .ready_windows
            .last()
            .map(|window| window.range)
            .or(self.source_window);
        let advances = frontier.is_none_or(|current| match direction {
            PlaybackDirection::Forward => range.end() > current.end(),
            PlaybackDirection::Reverse => range.start() < current.start(),
        });
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
