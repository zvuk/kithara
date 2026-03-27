#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test mock code; values are small and positive by construction"
)]

use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::time::Duration;

use crate::traits::PcmReader;
pub use crate::traits::{AudioEffectMock, PcmReaderMock};

/// A stateful `PcmReader` for testing facades that depend on audio playback.
///
/// Produces a constant sample value (0.5), tracks seek position and EOF,
/// and exposes an `EventBus` for event publishing in tests.
pub struct TestPcmReader {
    spec: PcmSpec,
    metadata: TrackMetadata,
    total_frames: u64,
    position_frames: u64,
    eof: bool,
    bus: EventBus,
}

impl TestPcmReader {
    /// Create a new test reader with the given spec and duration.
    #[must_use]
    pub fn new(spec: PcmSpec, duration_secs: f64) -> Self {
        let total_frames = (f64::from(spec.sample_rate) * duration_secs) as u64;
        Self {
            spec,
            metadata: TrackMetadata {
                album: None,
                artist: None,
                artwork: None,
                title: Some("Mock".to_owned()),
            },
            total_frames,
            position_frames: 0,
            eof: false,
            bus: EventBus::default(),
        }
    }

    /// Get a reference to the event bus for publishing mock events.
    #[must_use]
    pub fn event_bus(&self) -> &EventBus {
        &self.bus
    }

    fn frames_to_duration(&self, frames: u64) -> Duration {
        if self.spec.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate))
    }
}

const SAMPLE_VALUE: f32 = 0.5;

impl PcmReader for TestPcmReader {
    fn read(&mut self, buf: &mut [f32]) -> usize {
        if self.eof {
            return 0;
        }
        let channels = u64::from(self.spec.channels);
        if channels == 0 {
            return 0;
        }
        let remaining_samples = (self.total_frames - self.position_frames) * channels;
        let to_write = (buf.len() as u64).min(remaining_samples) as usize;
        for sample in &mut buf[..to_write] {
            *sample = SAMPLE_VALUE;
        }
        let frames_advanced = to_write as u64 / channels;
        self.position_frames += frames_advanced;
        if self.position_frames >= self.total_frames {
            self.eof = true;
        }
        to_write
    }

    fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize {
        if self.eof || output.is_empty() {
            return 0;
        }
        let channels = usize::from(self.spec.channels);
        if channels == 0 || output.len() < channels {
            return 0;
        }
        let frames_per_channel = output[0].len();
        let remaining = (self.total_frames - self.position_frames) as usize;
        let frames_to_write = frames_per_channel.min(remaining);
        for ch in output.iter_mut().take(channels) {
            for sample in ch.iter_mut().take(frames_to_write) {
                *sample = SAMPLE_VALUE;
            }
        }
        self.position_frames += frames_to_write as u64;
        if self.position_frames >= self.total_frames {
            self.eof = true;
        }
        frames_to_write
    }

    fn seek(&mut self, position: Duration) -> DecodeResult<()> {
        let frame = (position.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        self.position_frames = frame.min(self.total_frames);
        self.eof = self.position_frames >= self.total_frames;
        Ok(())
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn is_eof(&self) -> bool {
        self.eof
    }

    fn position(&self) -> Duration {
        self.frames_to_duration(self.position_frames)
    }

    fn duration(&self) -> Option<Duration> {
        Some(self.frames_to_duration(self.total_frames))
    }

    fn metadata(&self) -> &TrackMetadata {
        &self.metadata
    }

    fn event_bus(&self) -> &EventBus {
        &self.bus
    }
}
