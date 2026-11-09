use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use audioadapter_buffers::direct::InterleavedSlice;
use firewheel::{ActivateInfo, backend::BackendProcessInfo, node::StreamStatus};
use kithara_platform::sync::Arc;
use kithara_signal::AudioSpec;

use super::buffer::RingWriter;
use crate::session::{context::SessionGraph, processor::SessionProcessor};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum RingLayout {
    #[default]
    Stereo,
}

impl RingLayout {
    const fn channels(self) -> u16 {
        match self {
            Self::Stereo => 2,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct RingBackendProbe {
    inner: Arc<RingBackendProbeInner>,
}

#[derive(Default)]
struct RingBackendProbeInner {
    pre_arm_error: AtomicU8,
    starts: AtomicUsize,
}

impl RingBackendProbe {
    pub(crate) fn pre_arm_error(&self) -> Option<RingRenderError> {
        match self.inner.pre_arm_error.load(Ordering::SeqCst) {
            1 => Some(RingRenderError::NotArmed),
            2 => Some(RingRenderError::BlockShape),
            3 => Some(RingRenderError::Full),
            4 => Some(RingRenderError::FrameLedgerOverflow),
            _ => None,
        }
    }

    pub(crate) fn record_pre_arm_error(&self, error: RingRenderError) {
        let value = match error {
            RingRenderError::NotArmed => 1,
            RingRenderError::BlockShape => 2,
            RingRenderError::Full => 3,
            RingRenderError::FrameLedgerOverflow => 4,
        };
        self.inner.pre_arm_error.store(value, Ordering::SeqCst);
    }

    fn record_start(&self) {
        self.inner.starts.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn start_count(&self) -> usize {
        self.inner.starts.load(Ordering::SeqCst)
    }
}

#[non_exhaustive]
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(crate) struct RingBackendConfig {
    session_rate: NonZeroU32,
    writer: Option<RingWriter>,
    #[field(with, vis = "pub(crate)")]
    probe: RingBackendProbe,
    layout: RingLayout,
    block_frames: u32,
}

impl RingBackendConfig {
    #[must_use]
    pub(crate) fn new(session_rate: NonZeroU32, layout: RingLayout, writer: RingWriter) -> Self {
        let block_frames = writer.block_frames();
        Self {
            session_rate,
            block_frames,
            layout,
            probe: RingBackendProbe::default(),
            writer: Some(writer),
        }
    }
}

impl Default for RingBackendConfig {
    fn default() -> Self {
        Self {
            session_rate: NonZeroU32::new(44_100)
                .expect("invariant: default ring session rate is non-zero"),
            block_frames: 512,
            layout: RingLayout::Stereo,
            probe: RingBackendProbe::default(),
            writer: None,
        }
    }
}

pub(crate) struct RingBackend {
    processor: SessionProcessor,
    session_rate: NonZeroU32,
    layout: RingLayout,
    writer: RingWriter,
    armed: bool,
    block_frames: u32,
    committed_frames: u64,
    block_frames_usize: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum RingRenderError {
    #[error("ring backend is not armed")]
    NotArmed,
    #[error("ring block does not describe a valid interleaved buffer")]
    BlockShape,
    #[error("master ring is full")]
    Full,
    #[error("committed frame ledger overflow")]
    FrameLedgerOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum RingStartError {
    #[error("ring backend config has no writer")]
    MissingWriter,
    #[error("ring backend block size must be non-zero")]
    ZeroBlockFrames,
    #[error("ring backend block size does not fit usize")]
    BlockFramesOutOfRange,
    #[error("firewheel refused to activate the ring backend")]
    Activation,
}

impl RingBackend {
    pub(crate) const fn arm(&mut self) {
        self.armed = true;
    }

    #[must_use]
    pub(crate) const fn committed_frames(&self) -> u64 {
        self.committed_frames
    }

    pub(crate) fn render_block(&mut self, clock_samples: u64) -> Result<(), RingRenderError> {
        if !self.armed {
            return Err(RingRenderError::NotArmed);
        }
        let next_committed = self
            .committed_frames
            .checked_add(u64::from(self.block_frames))
            .ok_or(RingRenderError::FrameLedgerOverflow)?;
        let channels = self.layout.channels();
        let duration_since_stream_start = AudioSpec::new(channels, self.session_rate)
            .duration_for(clock_samples)
            .map_err(|_| RingRenderError::FrameLedgerOverflow)?;
        let process_info = BackendProcessInfo {
            frames: self.block_frames_usize,
            // Firewheel stamps a block with its own clock type, so the
            // platform clock cannot be handed over here.
            process_timestamp: Some(bevy_platform::time::Instant::now()),
            duration_since_stream_start,
            input_stream_status: StreamStatus::empty(),
            output_stream_status: StreamStatus::empty(),
            dropped_frames: 0,
            process_to_playback_delay: None,
        };
        {
            let (processor, writer) = (&mut self.processor, &mut self.writer);
            let mut block = writer
                .reserve(self.block_frames)
                .ok_or(RingRenderError::Full)?;
            let input = InterleavedSlice::new(&[] as &[f32], 0, 0)
                .map_err(|_| RingRenderError::BlockShape)?;
            let mut output = InterleavedSlice::new_mut(
                block.as_mut_slice(),
                usize::from(channels),
                self.block_frames_usize,
            )
            .map_err(|_| RingRenderError::BlockShape)?;
            processor.process(&input, &mut output, process_info);
            block.commit();
        }
        self.committed_frames = next_committed;
        Ok(())
    }

    /// Activates `cx` for manual rendering and takes the processor it returns.
    /// Nothing drives this stream but `render_block`, so the test decides when
    /// a block happens and what clock it happens at.
    pub(crate) fn start(
        cx: &mut SessionGraph,
        mut config: RingBackendConfig,
    ) -> Result<Self, RingStartError> {
        let max_block_frames =
            NonZeroU32::new(config.block_frames).ok_or(RingStartError::ZeroBlockFrames)?;
        let block_frames_usize = usize::try_from(config.block_frames)
            .map_err(|_| RingStartError::BlockFramesOutOfRange)?;
        let writer = config.writer.take().ok_or(RingStartError::MissingWriter)?;
        let channels = config.layout.channels();
        let processor = cx
            .activate(ActivateInfo {
                max_block_frames,
                sample_rate: config.session_rate,
                num_stream_in_channels: 0,
                num_stream_out_channels: u32::from(channels),
                input_to_output_latency_seconds: 0.0,
            })
            .map_err(|_| RingStartError::Activation)?;
        config.probe.record_start();
        Ok(Self {
            block_frames_usize,
            processor,
            writer,
            armed: false,
            block_frames: config.block_frames,
            committed_frames: 0,
            layout: config.layout,
            session_rate: config.session_rate,
        })
    }
}
