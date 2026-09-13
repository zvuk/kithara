use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use firewheel::{
    StreamInfo,
    backend::{AudioBackend, BackendProcessInfo},
    node::StreamStatus,
    processor::FirewheelProcessor,
};
use kithara::platform::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::buffer::RingWriter;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RingLayout {
    #[default]
    Stereo,
}

impl RingLayout {
    const fn channels(self) -> usize {
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
    pub(crate) fn start_count(&self) -> usize {
        self.inner.starts.load(Ordering::SeqCst)
    }

    pub(crate) fn pre_arm_error(&self) -> Option<RingRenderError> {
        match self.inner.pre_arm_error.load(Ordering::SeqCst) {
            1 => Some(RingRenderError::NotArmed),
            2 => Some(RingRenderError::MissingProcessor),
            3 => Some(RingRenderError::Full),
            4 => Some(RingRenderError::FrameLedgerOverflow),
            _ => None,
        }
    }

    pub(crate) fn record_pre_arm_error(&self, error: RingRenderError) {
        let value = match error {
            RingRenderError::NotArmed => 1,
            RingRenderError::MissingProcessor => 2,
            RingRenderError::Full => 3,
            RingRenderError::FrameLedgerOverflow => 4,
        };
        self.inner.pre_arm_error.store(value, Ordering::SeqCst);
    }

    fn record_start(&self) {
        self.inner.starts.fetch_add(1, Ordering::SeqCst);
    }
}

#[non_exhaustive]
pub struct RingBackendConfig {
    session_rate: NonZeroU32,
    block_frames: u32,
    layout: RingLayout,
    probe: RingBackendProbe,
    writer: Option<RingWriter>,
}

impl RingBackendConfig {
    #[must_use]
    pub fn new(session_rate: NonZeroU32, layout: RingLayout, writer: RingWriter) -> Self {
        let block_frames = writer.block_frames();
        Self {
            session_rate,
            block_frames,
            layout,
            probe: RingBackendProbe::default(),
            writer: Some(writer),
        }
    }

    #[must_use]
    pub(crate) fn with_probe(mut self, probe: RingBackendProbe) -> Self {
        self.probe = probe;
        self
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

pub struct RingBackend {
    armed: bool,
    block_frames: u32,
    block_frames_usize: usize,
    committed_frames: u64,
    layout: RingLayout,
    processor: Option<FirewheelProcessor<Self>>,
    session_rate: NonZeroU32,
    writer: RingWriter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RingRenderError {
    #[error("ring backend is not armed")]
    NotArmed,
    #[error("ring backend has no firewheel processor")]
    MissingProcessor,
    #[error("master ring is full")]
    Full,
    #[error("committed frame ledger overflow")]
    FrameLedgerOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RingStartError {
    #[error("ring backend config has no writer")]
    MissingWriter,
    #[error("ring backend block size must be non-zero")]
    ZeroBlockFrames,
    #[error("ring backend block size does not fit usize")]
    BlockFramesOutOfRange,
}

#[derive(Debug, thiserror::Error)]
#[error("ring backend stream failed")]
pub struct RingStreamError;

impl AudioBackend for RingBackend {
    type Config = RingBackendConfig;
    type Enumerator = ();
    type Instant = Instant;
    type StartStreamError = RingStartError;
    type StreamError = RingStreamError;

    fn delay_from_last_process(&self, _process_timestamp: Self::Instant) -> Option<Duration> {
        None
    }

    fn enumerator() -> Self::Enumerator {}

    fn poll_status(&mut self) -> Result<(), Self::StreamError> {
        Ok(())
    }

    fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
        self.processor = Some(processor);
    }

    fn start_stream(
        mut config: Self::Config,
    ) -> Result<(Self, StreamInfo), Self::StartStreamError> {
        let max_block_frames =
            NonZeroU32::new(config.block_frames).ok_or(RingStartError::ZeroBlockFrames)?;
        let block_frames_usize = usize::try_from(config.block_frames)
            .map_err(|_| RingStartError::BlockFramesOutOfRange)?;
        let writer = config.writer.take().ok_or(RingStartError::MissingWriter)?;
        let channels = config.layout.channels();
        let stream_info = StreamInfo {
            sample_rate: config.session_rate,
            sample_rate_recip: f64::from(config.session_rate.get()).recip(),
            prev_sample_rate: config.session_rate,
            max_block_frames,
            num_stream_in_channels: 0,
            num_stream_out_channels: channels as u32,
            input_to_output_latency_seconds: 0.0,
            declick_frames: max_block_frames,
            output_device_id: String::from("manual-master-ring"),
            input_device_id: None,
        };
        config.probe.record_start();
        Ok((
            Self {
                armed: false,
                block_frames: config.block_frames,
                block_frames_usize,
                committed_frames: 0,
                layout: config.layout,
                processor: None,
                session_rate: config.session_rate,
                writer,
            },
            stream_info,
        ))
    }
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
        let process_info = BackendProcessInfo {
            num_in_channels: 0,
            num_out_channels: channels,
            frames: self.block_frames_usize,
            process_timestamp: Instant::now(),
            duration_since_stream_start: Duration::from_secs_f64(
                clock_samples as f64 / f64::from(self.session_rate.get()),
            ),
            input_stream_status: StreamStatus::empty(),
            output_stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        {
            let (processor, writer) = (&mut self.processor, &mut self.writer);
            let mut block = writer
                .reserve(self.block_frames)
                .ok_or(RingRenderError::Full)?;
            let processor = processor
                .as_mut()
                .ok_or(RingRenderError::MissingProcessor)?;
            processor.process_interleaved(&[], block.as_mut_slice(), process_info);
            block.commit();
        }
        self.committed_frames = next_committed;
        Ok(())
    }
}
