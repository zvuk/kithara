mod allocation;
mod preparation;
mod relocation;
mod rendering;
mod windows;

use std::{collections::VecDeque, mem::replace, num::NonZeroU32};

use kithara_bufpool::PcmBuf;
use kithara_events::PlaybackDirection;
use kithara_stretch::{
    ElasticCapabilities, ElasticConfig, ElasticCursor, ElasticRequest, ElasticSpanConfig,
    SignalsmithBackend,
};

use super::{ElasticReaderConfig, ElasticReaderError};
use crate::{SourceRange, SourceRangeRequest};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ElasticCopyError {
    FrameOverflow,
    FetchWindowMismatch,
}

impl From<ElasticCopyError> for ElasticReaderError {
    fn from(error: ElasticCopyError) -> Self {
        match error {
            ElasticCopyError::FrameOverflow => Self::FrameOverflow,
            ElasticCopyError::FetchWindowMismatch => Self::FetchWindowMismatch,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Outcome of one exact-span read into caller-owned planar output.
pub enum ElasticReadOutcome {
    /// The requested output frames were rendered.
    Ready { frames: usize },
    /// The requested source path lies beyond the decoded track extent.
    Eof,
}

#[derive(Clone, Copy)]
struct ElasticPreparation {
    anchor: ElasticCursor,
    warmup: ElasticRequest,
    direction: PlaybackDirection,
    fetch_range: SourceRange,
}

#[derive(Clone, Copy)]
struct PreparedRuntime {
    cursor: ElasticCursor,
    direction: PlaybackDirection,
    source_window: SourceRange,
}

struct RenderRuntime {
    pending_source_read: Option<PendingSourceRead>,
    relocation: Option<ElasticRelocation>,
    prepared: PreparedRuntime,
}

struct ElasticRelocation {
    preparation: ElasticPreparation,
    read: RelocationRead,
}

enum RelocationRead {
    Idle,
    Pending(PendingSourceRead),
    Ready(PcmBuf),
}

impl RelocationRead {
    fn take_samples(&mut self) -> Option<PcmBuf> {
        match replace(self, Self::Idle) {
            Self::Pending(pending) => Some(pending.samples),
            Self::Ready(samples) => Some(samples),
            Self::Idle => None,
        }
    }
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

/// Reader state before source preparation begins.
pub struct Unprepared;

/// Reader state while bounded source ranges are being prepared.
pub struct Preparing {
    preparation: ElasticPreparation,
    phase: PreparationPhase,
}

/// Reader state prepared for activation without callback ownership.
pub struct Ready {
    runtime: RenderRuntime,
}

/// Reader state owned by an active callback path.
pub struct Active {
    runtime: RenderRuntime,
}

/// Result of polling off-callback source preparation.
pub enum ElasticPreparationPoll {
    /// Preparation needs another non-blocking source poll.
    Pending(ElasticReader<Preparing>),
    /// The reader is fully primed and can be activated.
    Ready(ElasticReader<Ready>),
}

struct BufferedSourceWindow {
    samples: PcmBuf,
    range: SourceRange,
}

struct PendingSourceRead {
    samples: PcmBuf,
    request: SourceRangeRequest,
}

/// Typestate reader joining bounded `PcmReader` ranges to exact-span DSP.
pub struct ElasticReader<State> {
    config: ElasticReaderConfig,
    span_config: ElasticSpanConfig,
    sample_rate: NonZeroU32,
    relocation_buffer: Option<PcmBuf>,
    discarded: PcmBuf,
    fetch: PcmBuf,
    history: PcmBuf,
    output: PcmBuf,
    source: PcmBuf,
    backend: SignalsmithBackend<ElasticConfig>,
    state: State,
    window_buffers: Vec<PcmBuf>,
    ready_windows: VecDeque<BufferedSourceWindow>,
    source_frame_count: u64,
    source_window_frames: u64,
    max_fetch_frames: usize,
    max_source_frames: usize,
    max_warm_frames: usize,
    output_channels: usize,
}

impl<State> ElasticReader<State> {
    /// Immutable DSP limits used by transport-side span planning.
    #[must_use]
    pub const fn capabilities(&self) -> ElasticCapabilities {
        self.backend.capabilities()
    }

    fn map_state<Next>(self, map: impl FnOnce(State) -> Next) -> ElasticReader<Next> {
        let Self {
            config,
            span_config,
            sample_rate,
            relocation_buffer,
            discarded,
            fetch,
            history,
            output,
            source,
            backend,
            state,
            window_buffers,
            ready_windows,
            source_frame_count,
            source_window_frames,
            max_fetch_frames,
            max_source_frames,
            max_warm_frames,
            output_channels,
        } = self;
        ElasticReader {
            config,
            span_config,
            sample_rate,
            relocation_buffer,
            discarded,
            fetch,
            history,
            output,
            source,
            backend,
            window_buffers,
            ready_windows,
            source_frame_count,
            source_window_frames,
            max_fetch_frames,
            max_source_frames,
            max_warm_frames,
            output_channels,
            state: map(state),
        }
    }
}

fn sample_count(frames: usize, channels: usize) -> Result<usize, ElasticReaderError> {
    frames
        .checked_mul(channels)
        .ok_or(ElasticReaderError::FrameOverflow)
}
