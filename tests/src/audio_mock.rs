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
    audio::{
        AudioControl, AudioRead, AudioSession, ConsumerWakeMode, PendingReason, ReadOutcome,
        SeekBegin, SeekOutcome,
    },
    decode::{DecodeError, TrackMetadata},
    events::EventBus,
    platform::time::Duration,
    signal::AudioSpec,
};

/// A stateful fixed-rate `AudioReader` for testing playback facades.
pub struct TestPcmReader {
    bus: EventBus,
    spec: AudioSpec,
    metadata: TrackMetadata,
    position_frames: u64,
    total_frames: u64,
    source: Source,
}

enum Source {
    Samples(Vec<f32>),
    Bytes(&'static [u8]),
}

/// Expected amplitude of the default prepared PCM fixture.
pub const TEST_PCM_DEFAULT_VALUE: f32 = 0.5;

impl TestPcmReader {
    #[must_use]
    pub fn from_samples(spec: AudioSpec, samples: Vec<f32>) -> Self {
        let total_frames = samples.len() as u64;
        let mut reader = Self::with_source(spec, 0.0, Source::Samples(samples));
        reader.total_frames = total_frames;
        reader
    }

    #[must_use]
    pub fn from_pcm(spec: AudioSpec, duration_secs: f64, bytes: &'static [u8]) -> Self {
        assert!(
            bytes.len().is_multiple_of(size_of::<f32>()),
            "prepared PCM must contain whole samples"
        );
        let reader = Self::with_source(spec, duration_secs, Source::Bytes(bytes));
        assert!(
            reader.total_frames <= (bytes.len() / size_of::<f32>()) as u64,
            "prepared PCM is shorter than the requested track"
        );
        reader
    }

    fn with_source(spec: AudioSpec, duration_secs: f64, source: Source) -> Self {
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
            source,
        }
    }

    fn sample_at(&self, start: u64, output_frame: u64) -> f32 {
        match &self.source {
            Source::Samples(samples) => samples[(start + output_frame) as usize],
            Source::Bytes(bytes) => prepared_sample(bytes, (start + output_frame) as usize),
        }
    }

    const fn at_natural_end(&self) -> bool {
        self.position_frames >= self.total_frames
    }

    /// Output frames still renderable before the source budget runs out.
    fn output_frames_left(&self) -> u64 {
        self.total_frames - self.position_frames
    }

    /// Advance the source cursor by the frames consumed to render
    /// `output_frames`, saturating at the total budget.
    fn consume(&mut self, output_frames: u64) {
        self.position_frames = self
            .position_frames
            .saturating_add(output_frames)
            .min(self.total_frames);
    }

    fn eof_outcome(&self) -> ReadOutcome {
        ReadOutcome::Eof {
            position: self.frames_to_duration(self.position_frames),
        }
    }

    /// Get a reference to the event bus for publishing mock events.
    #[must_use]
    pub const fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn frames_to_duration(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate.get()))
    }
}

impl AudioSession for TestPcmReader {
    fn duration(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.total_frames))
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for TestPcmReader {
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
        let renderable_samples = self.output_frames_left() * channels;
        let to_write = (buf.len() as u64).min(renderable_samples) as usize;
        let start = self.position_frames;
        for (index, sample) in buf[..to_write].iter_mut().enumerate() {
            *sample = self.sample_at(start, index as u64 / channels);
        }
        self.consume(to_write as u64 / channels);
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
            source_span: None,
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
        let renderable = self.output_frames_left() as usize;
        let frames_to_write = frames_per_channel.min(renderable);
        let start = self.position_frames;
        for ch in output.iter_mut().take(channels) {
            for (frame, sample) in ch.iter_mut().take(frames_to_write).enumerate() {
                *sample = self.sample_at(start, frame as u64);
            }
        }
        self.consume(frames_to_write as u64);
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
            source_span: None,
        })
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for TestPcmReader {
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
}

pub struct MockReader {
    behavior: MockBehavior,
    bus: EventBus,
    metadata: TrackMetadata,
    spec: AudioSpec,
}

enum MockBehavior {
    /// Records the session-owned capabilities an owner applies to the reader.
    AdoptionTracking {
        recorded_host_rate: Arc<AtomicU32>,
        recorded_wake_mode: Arc<Mutex<Option<ConsumerWakeMode>>>,
        duration: Duration,
    },
    SeekTracking {
        seek_log: Arc<Mutex<Vec<u64>>>,
    },
    MisreportedDuration {
        position_frames: usize,
        remaining_frames: usize,
        samples: &'static [u8],
    },
    LiveFrontier {
        frontier_ns: Arc<AtomicU64>,
    },
    Faulty(Fault),
    SeekSplit(SeekSplitCounts),
}

fn adoption_tracking(recorded_host_rate: Arc<AtomicU32>, duration: Duration) -> MockBehavior {
    MockBehavior::AdoptionTracking {
        recorded_host_rate,
        recorded_wake_mode: Arc::new(Mutex::new(None)),
        duration,
    }
}

impl MockReader {
    fn with_behavior(spec: AudioSpec, behavior: MockBehavior) -> Self {
        Self {
            behavior,
            bus: EventBus::default(),
            metadata: TrackMetadata::default(),
            spec,
        }
    }

    #[must_use]
    pub fn sample_rate_tracking(spec: AudioSpec) -> (Self, Arc<AtomicU32>) {
        Self::sample_rate_tracking_with_duration(spec, Duration::from_secs(60))
    }

    #[must_use]
    pub fn sample_rate_tracking_with_duration(
        spec: AudioSpec,
        duration: Duration,
    ) -> (Self, Arc<AtomicU32>) {
        let recorded = Arc::new(AtomicU32::new(0));
        let reader = Self::with_behavior(spec, adoption_tracking(Arc::clone(&recorded), duration));
        (reader, recorded)
    }

    /// Reader recording the consumer wake mode its owner applies to it.
    #[must_use]
    pub fn wake_mode_tracking(spec: AudioSpec) -> (Self, Arc<Mutex<Option<ConsumerWakeMode>>>) {
        let behavior = adoption_tracking(Arc::new(AtomicU32::new(0)), Duration::from_secs(60));
        let MockBehavior::AdoptionTracking {
            ref recorded_wake_mode,
            ..
        } = behavior
        else {
            unreachable!("adoption tracking builds one variant")
        };
        let recorded = Arc::clone(recorded_wake_mode);
        (Self::with_behavior(spec, behavior), recorded)
    }

    #[must_use]
    pub fn seek_tracking(seek_log: Arc<Mutex<Vec<u64>>>) -> Self {
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut reader = Self::with_behavior(spec, MockBehavior::SeekTracking { seek_log });
        reader.metadata.title = Some("Tracking".to_owned());
        reader
    }

    #[must_use]
    pub fn misreported_duration(
        spec: AudioSpec,
        actual_frames: usize,
        samples: &'static [u8],
    ) -> Self {
        Self::with_behavior(
            spec,
            MockBehavior::MisreportedDuration {
                position_frames: 0,
                remaining_frames: actual_frames,
                samples,
            },
        )
    }

    #[must_use]
    pub fn live_frontier(spec: AudioSpec, frontier_ns: Arc<AtomicU64>) -> Self {
        Self::with_behavior(spec, MockBehavior::LiveFrontier { frontier_ns })
    }

    #[must_use]
    pub fn faulty(spec: AudioSpec, fault: Fault) -> Self {
        Self::with_behavior(spec, MockBehavior::Faulty(fault))
    }

    #[must_use]
    pub fn seek_split(spec: AudioSpec) -> (Self, SeekSplitCounts) {
        let counts = SeekSplitCounts::default();
        let reader = Self::with_behavior(spec, MockBehavior::SeekSplit(counts.clone()));
        (reader, counts)
    }

    fn fixed_outcome(&self) -> Result<ReadOutcome, DecodeError> {
        match &self.behavior {
            MockBehavior::LiveFrontier { .. } => Ok(ReadOutcome::Eof {
                position: Duration::ZERO,
            }),
            MockBehavior::Faulty(Fault::DecodeError) => Err(DecodeError::Io {
                source: std::io::Error::other("mock decode failure"),
            }),
            MockBehavior::Faulty(Fault::Stall | Fault::RefuseSeek)
            | MockBehavior::AdoptionTracking { .. }
            | MockBehavior::SeekTracking { .. }
            | MockBehavior::SeekSplit(_) => Ok(ReadOutcome::Pending {
                position: Duration::ZERO,
                reason: PendingReason::Buffering,
            }),
            MockBehavior::MisreportedDuration { .. } => unreachable!(),
        }
    }

    fn position_for(spec: AudioSpec, frames: usize) -> Duration {
        let frames = u64::try_from(frames).expect("test mock position non-negative");
        Duration::from_micros(frames * 1_000_000 / u64::from(spec.sample_rate.get()))
    }
}

impl AudioSession for MockReader {
    fn duration(&self) -> Option<Duration> {
        match &self.behavior {
            MockBehavior::AdoptionTracking { duration, .. } => Some(*duration),
            MockBehavior::SeekTracking { .. } => None,
            MockBehavior::MisreportedDuration { .. } => Some(Duration::from_secs(10)),
            MockBehavior::LiveFrontier { .. } => Some(Duration::from_secs(180)),
            MockBehavior::Faulty(_) | MockBehavior::SeekSplit(_) => Some(Duration::from_secs(60)),
        }
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }
}

impl AudioRead for MockReader {
    fn decoded_frontier(&self) -> Duration {
        match &self.behavior {
            MockBehavior::LiveFrontier { frontier_ns } => {
                Duration::from_nanos(frontier_ns.load(Ordering::Relaxed))
            }
            _ => Duration::ZERO,
        }
    }

    fn position(&self) -> Duration {
        match &self.behavior {
            MockBehavior::MisreportedDuration {
                position_frames, ..
            } => Self::position_for(self.spec, *position_frames),
            _ => Duration::ZERO,
        }
    }

    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        let MockBehavior::MisreportedDuration {
            position_frames,
            remaining_frames,
            samples,
        } = &mut self.behavior
        else {
            return self.fixed_outcome();
        };
        let channels = usize::from(self.spec.channels);
        let frames = (buf.len() / channels).min(*remaining_frames);
        if frames == 0 {
            return Ok(ReadOutcome::Eof {
                position: Self::position_for(self.spec, *position_frames),
            });
        }
        for (index, sample) in buf[..frames.saturating_mul(channels)]
            .iter_mut()
            .enumerate()
        {
            *sample = prepared_sample(samples, *position_frames + index / channels);
        }
        *remaining_frames -= frames;
        *position_frames += frames;
        Ok(ReadOutcome::Frames {
            count: NonZeroUsize::new(frames).expect("BUG: frames > 0"),
            position: Self::position_for(self.spec, *position_frames),
            source_span: None,
        })
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        let MockBehavior::MisreportedDuration {
            position_frames,
            remaining_frames,
            samples,
        } = &mut self.behavior
        else {
            return self.fixed_outcome();
        };
        let frames = output
            .iter()
            .map(|channel| channel.len())
            .min()
            .unwrap_or(0)
            .min(*remaining_frames);
        if frames == 0 {
            return Ok(ReadOutcome::Eof {
                position: Self::position_for(self.spec, *position_frames),
            });
        }
        for channel in output {
            for (frame, sample) in channel[..frames].iter_mut().enumerate() {
                *sample = prepared_sample(samples, *position_frames + frame);
            }
        }
        *remaining_frames -= frames;
        *position_frames += frames;
        Ok(ReadOutcome::Frames {
            count: NonZeroUsize::new(frames).expect("BUG: frames > 0"),
            position: Self::position_for(self.spec, *position_frames),
            source_span: None,
        })
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for MockReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        match &mut self.behavior {
            MockBehavior::SeekTracking { seek_log } => {
                let ms = u64::try_from(position.as_millis()).expect("test seek fits in u64");
                seek_log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(ms);
            }
            MockBehavior::Faulty(Fault::RefuseSeek) => {
                return Err(DecodeError::Io {
                    source: std::io::Error::other("mock seek refusal"),
                });
            }
            MockBehavior::SeekSplit(counts) => {
                counts.blocking_seeks.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        Ok(SeekOutcome::Landed {
            target: position,
            landed_at: position,
        })
    }

    fn set_consumer_wake_mode(&mut self, mode: ConsumerWakeMode) {
        if let MockBehavior::AdoptionTracking {
            recorded_wake_mode, ..
        } = &self.behavior
        {
            *recorded_wake_mode
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mode);
        }
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        if let MockBehavior::AdoptionTracking {
            recorded_host_rate, ..
        } = &self.behavior
        {
            recorded_host_rate.store(sample_rate.get(), Ordering::Relaxed);
        }
    }

    fn seek_handle(&self) -> Option<Arc<dyn SeekBegin>> {
        match &self.behavior {
            MockBehavior::SeekSplit(counts) => Some(Arc::new(SeekSpy(counts.clone()))),
            _ => None,
        }
    }

    fn sync_seek(&mut self) {
        if let MockBehavior::SeekSplit(counts) = &self.behavior {
            counts.syncs.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// How a faulty `MockReader` misbehaves, so RT paths that must not log or block can be driven into
/// their failure branches from a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Every read returns a decoder error.
    DecodeError,
    /// Every read reports no frames without reaching EOF — the underrun path.
    Stall,
    /// Reads succeed; seeks are refused.
    RefuseSeek,
}

/// Counts which half of a seek each caller used, so a test can pin that the audio thread never runs
/// the blocking one.
#[derive(Clone, Default)]
pub struct SeekSplitCounts {
    pub blocking_seeks: Arc<AtomicU64>,
    pub begins: Arc<AtomicU64>,
    pub syncs: Arc<AtomicU64>,
}

impl SeekSplitCounts {
    #[must_use]
    pub fn blocking_seeks(&self) -> u64 {
        self.blocking_seeks.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn begins(&self) -> u64 {
        self.begins.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn syncs(&self) -> u64 {
        self.syncs.load(Ordering::Relaxed)
    }
}

struct SeekSpy(SeekSplitCounts);

impl SeekBegin for SeekSpy {
    fn begin(&self, position: Duration) -> SeekOutcome {
        self.0.begins.fetch_add(1, Ordering::Relaxed);
        SeekOutcome::Landed {
            target: position,
            landed_at: position,
        }
    }
}

fn prepared_sample(bytes: &[u8], frame: usize) -> f32 {
    let index = frame * size_of::<f32>();
    f32::from_le_bytes(
        bytes[index..index + size_of::<f32>()]
            .try_into()
            .expect("prepared PCM sample"),
    )
}
