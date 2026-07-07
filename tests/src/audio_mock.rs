#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test mock code; values are small and positive by construction"
)]

use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use kithara::{
    audio::{PcmReader, PendingReason, ReadOutcome, SeekOutcome},
    decode::{DecodeError, PcmSpec, TrackMetadata},
    events::EventBus,
    platform::time::Duration,
};

/// A stateful `PcmReader` for testing facades that depend on audio playback.
///
/// Produces a constant sample value (0.5), tracks seek position and reports
/// [`ReadOutcome::Eof`] once the total-frame budget is consumed.
pub struct TestPcmReader {
    bus: EventBus,
    spec: PcmSpec,
    metadata: TrackMetadata,
    position_frames: u64,
    total_frames: u64,
    value: f32,
}

/// Default sample value emitted by [`TestPcmReader::new`].
pub const TEST_PCM_DEFAULT_VALUE: f32 = 0.5;

impl TestPcmReader {
    /// Create a new test reader with the given spec and duration.
    /// Emits [`TEST_PCM_DEFAULT_VALUE`] for every sample.
    #[must_use]
    pub fn new(spec: PcmSpec, duration_secs: f64) -> Self {
        Self::with_value(spec, duration_secs, TEST_PCM_DEFAULT_VALUE)
    }

    /// Create a test reader emitting the given constant `value` for every
    /// sample. Distinguishable per-track values let integration tests
    /// verify which track a rendered PCM window belongs to.
    #[must_use]
    pub fn with_value(spec: PcmSpec, duration_secs: f64, value: f32) -> Self {
        let total_frames = (f64::from(spec.sample_rate.get()) * duration_secs) as u64;
        Self {
            spec,
            total_frames,
            metadata: TrackMetadata {
                title: Some("Mock".to_owned()),
                ..TrackMetadata::default()
            },
            position_frames: 0,
            bus: EventBus::default(),
            value,
        }
    }

    fn at_natural_end(&self) -> bool {
        self.position_frames >= self.total_frames
    }

    fn eof_outcome(&self) -> ReadOutcome {
        ReadOutcome::Eof {
            position: self.frames_to_duration(self.position_frames),
        }
    }

    /// Get a reference to the event bus for publishing mock events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn frames_to_duration(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate.get()))
    }
}

impl PcmReader for TestPcmReader {
    fn duration(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.total_frames))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn position(&self) -> Duration {
        self.frames_to_duration(self.position_frames)
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        if self.at_natural_end() {
            return Ok(self.eof_outcome());
        }
        let channels = u64::from(self.spec.channels);
        let position = self.frames_to_duration(self.position_frames);
        if channels == 0 || buf.is_empty() {
            return Ok(ReadOutcome::Pending {
                position,
                reason: PendingReason::Buffering,
            });
        }
        let remaining_samples = (self.total_frames - self.position_frames) * channels;
        let to_write = (buf.len() as u64).min(remaining_samples) as usize;
        for sample in &mut buf[..to_write] {
            *sample = self.value;
        }
        let frames_advanced = to_write as u64 / channels;
        self.position_frames += frames_advanced;
        let new_position = self.frames_to_duration(self.position_frames);
        let Some(count) = NonZeroUsize::new(to_write) else {
            return Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: new_position,
            });
        };
        Ok(ReadOutcome::Frames {
            count,
            position: new_position,
        })
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        if self.at_natural_end() {
            return Ok(self.eof_outcome());
        }
        let position = self.frames_to_duration(self.position_frames);
        if output.is_empty() {
            return Ok(ReadOutcome::Pending {
                position,
                reason: PendingReason::Buffering,
            });
        }
        let channels = usize::from(self.spec.channels);
        if channels == 0 || output.len() < channels {
            return Ok(ReadOutcome::Pending {
                position,
                reason: PendingReason::Buffering,
            });
        }
        let frames_per_channel = output[0].len();
        let remaining = (self.total_frames - self.position_frames) as usize;
        let frames_to_write = frames_per_channel.min(remaining);
        for ch in output.iter_mut().take(channels) {
            for sample in ch.iter_mut().take(frames_to_write) {
                *sample = self.value;
            }
        }
        self.position_frames += frames_to_write as u64;
        let new_position = self.frames_to_duration(self.position_frames);
        let Some(count) = NonZeroUsize::new(frames_to_write) else {
            return Ok(ReadOutcome::Pending {
                reason: PendingReason::Buffering,
                position: new_position,
            });
        };
        Ok(ReadOutcome::Frames {
            count,
            position: new_position,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let target = position;
        let frame = (position.as_secs_f64() * f64::from(self.spec.sample_rate.get())) as u64;
        self.position_frames = frame.min(self.total_frames);
        let landed_at = self.frames_to_duration(self.position_frames);
        if let Some(duration) = self.duration()
            && position >= duration
        {
            return Ok(SeekOutcome::PastEof { target, duration });
        }
        Ok(SeekOutcome::Landed { target, landed_at })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

pub struct SampleRateTrackingReader {
    recorded_host_rate: Arc<AtomicU32>,
    bus: EventBus,
    duration: Duration,
    metadata: TrackMetadata,
    spec: PcmSpec,
}

impl SampleRateTrackingReader {
    #[must_use]
    pub fn new(spec: PcmSpec) -> (Self, Arc<AtomicU32>) {
        Self::with_duration(spec, Duration::from_secs(60))
    }

    #[must_use]
    pub fn with_duration(spec: PcmSpec, duration: Duration) -> (Self, Arc<AtomicU32>) {
        let recorded = Arc::new(AtomicU32::new(0));
        let reader = Self {
            bus: EventBus::default(),
            duration,
            metadata: TrackMetadata::default(),
            recorded_host_rate: Arc::clone(&recorded),
            spec,
        };
        (reader, recorded)
    }
}

impl PcmReader for SampleRateTrackingReader {
    fn decoded_frontier(&self) -> Duration {
        Duration::ZERO
    }

    fn duration(&self) -> Option<Duration> {
        Some(self.duration)
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            position: Duration::ZERO,
            reason: PendingReason::Buffering,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            position: Duration::ZERO,
            reason: PendingReason::Buffering,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.recorded_host_rate
            .store(sample_rate.get(), Ordering::Relaxed);
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

pub struct SeekTrackingReader {
    bus: EventBus,
    metadata: TrackMetadata,
    seek_log: Arc<Mutex<Vec<u64>>>,
    spec: PcmSpec,
}

impl SeekTrackingReader {
    #[must_use]
    pub fn new(seek_log: Arc<Mutex<Vec<u64>>>) -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata {
                title: Some("Tracking".to_owned()),
                ..TrackMetadata::default()
            },
            seek_log,
            spec: PcmSpec::new(2, NonZeroU32::new(44100).expect("test rate")),
        }
    }
}

impl PcmReader for SeekTrackingReader {
    fn duration(&self) -> Option<Duration> {
        None
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            position: Duration::ZERO,
            reason: PendingReason::Buffering,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Pending {
            position: Duration::ZERO,
            reason: PendingReason::Buffering,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let ms = u64::try_from(position.as_millis()).expect("test seek fits in u64");
        self.seek_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ms);
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

pub struct MisreportedDurationReader {
    bus: EventBus,
    spec: PcmSpec,
    metadata: TrackMetadata,
    position_frames: usize,
    remaining_frames: usize,
}

impl MisreportedDurationReader {
    #[must_use]
    pub fn new(spec: PcmSpec, actual_frames: usize) -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            remaining_frames: actual_frames,
            position_frames: 0,
            spec,
        }
    }
}

impl PcmReader for MisreportedDurationReader {
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(10))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn position(&self) -> Duration {
        let frames = u64::try_from(self.position_frames).expect("test mock position non-negative");
        Duration::from_micros(frames * 1_000_000 / u64::from(self.spec.sample_rate.get()))
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let channels = usize::from(self.spec.channels);
        let frames = (buf.len() / channels).min(self.remaining_frames);
        if frames == 0 {
            return Ok(ReadOutcome::Eof {
                position: self.position(),
            });
        }
        let samples = frames.saturating_mul(channels);
        buf[..samples].fill(0.5);
        self.remaining_frames -= frames;
        self.position_frames += frames;
        Ok(ReadOutcome::Frames {
            count: NonZeroUsize::new(frames).expect("BUG: frames > 0"),
            position: self.position(),
        })
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let frames = output
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0)
            .min(self.remaining_frames);
        if frames == 0 {
            return Ok(ReadOutcome::Eof {
                position: self.position(),
            });
        }
        for channel in output.iter_mut() {
            channel[..frames].fill(0.5);
        }
        self.remaining_frames -= frames;
        self.position_frames += frames;
        Ok(ReadOutcome::Frames {
            count: NonZeroUsize::new(frames).expect("BUG: frames > 0"),
            position: self.position(),
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

pub struct LiveFrontierReader {
    frontier_ns: Arc<AtomicU64>,
    bus: EventBus,
    spec: PcmSpec,
    metadata: TrackMetadata,
}

impl LiveFrontierReader {
    #[must_use]
    pub fn new(spec: PcmSpec, frontier_ns: Arc<AtomicU64>) -> Self {
        Self {
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            frontier_ns,
            spec,
        }
    }
}

impl PcmReader for LiveFrontierReader {
    fn decoded_frontier(&self) -> Duration {
        Duration::from_nanos(self.frontier_ns.load(Ordering::Relaxed))
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(180))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn read(&mut self, _buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn read_planar<'a>(
        &mut self,
        _output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        Ok(ReadOutcome::Eof {
            position: Duration::ZERO,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}
