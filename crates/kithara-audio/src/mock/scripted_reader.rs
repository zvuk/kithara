use std::{
    num::{NonZeroU32, NonZeroUsize},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{
    sync::{Arc, Mutex},
    time::Duration,
};
use kithara_signal::AudioSpec;

use super::pcm_reader::prepared_sample;
use crate::{
    AudioControl, AudioRead, AudioSession, ConsumerWakeMode, PendingReason, ReadOutcome, SeekBegin,
    SeekOutcome,
};

/// Sample rate of the reader [`MockReader::seek_tracking`] builds.
const SEEK_TRACKING_RATE: NonZeroU32 = match NonZeroU32::new(44_100) {
    Some(rate) => rate,
    None => panic!("seek-tracking rate is non-zero"),
};

/// A reader scripted for one behaviour an owner must handle: recording what
/// the owner applies, lying about its duration, stalling, failing, or
/// splitting its seek.
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
        let spec = AudioSpec::new(2, SEEK_TRACKING_RATE);
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
                seek_log.lock().push(ms);
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
            *recorded_wake_mode.lock() = Some(mode);
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
