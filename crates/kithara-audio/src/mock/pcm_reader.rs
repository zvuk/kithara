use std::num::NonZeroUsize;

use kithara_decode::{DecodeError, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::{AudioControl, AudioRead, AudioSession, PendingReason, ReadOutcome, SeekOutcome};

/// Expected amplitude of the default prepared PCM fixture.
pub const TEST_PCM_DEFAULT_VALUE: f32 = 0.5;

/// A stateful fixed-rate reader for testing playback facades.
///
/// Every channel of output frame `n` carries source sample `n`, so a
/// constant source renders the same value on every channel.
pub struct TestPcmReader {
    bus: EventBus,
    spec: AudioSpec,
    metadata: TrackMetadata,
    position_frames: usize,
    total_frames: usize,
    source: Source,
}

enum Source {
    Samples(Vec<f32>),
    Bytes(&'static [u8]),
}

impl TestPcmReader {
    /// A reader of `duration_secs` of [`TEST_PCM_DEFAULT_VALUE`].
    ///
    /// # Panics
    ///
    /// Panics when the duration has no frame count at the spec's rate.
    #[must_use]
    pub fn new(spec: AudioSpec, duration_secs: f64) -> Self {
        let frames = frames_for(spec, duration_secs);
        Self::with_samples(
            spec,
            vec![TEST_PCM_DEFAULT_VALUE; frames * usize::from(spec.channels)],
        )
    }

    /// A reader whose track is exactly `samples`, one sample per frame.
    #[must_use]
    pub fn with_samples(spec: AudioSpec, samples: Vec<f32>) -> Self {
        let total_frames = samples.len();
        let mut reader = Self::build(spec, 0, Source::Samples(samples));
        reader.total_frames = total_frames;
        reader
    }

    /// A reader of `duration_secs` of prepared little-endian `f32` PCM.
    ///
    /// # Panics
    ///
    /// Panics when `bytes` holds a partial sample or fewer samples than the
    /// requested track.
    #[must_use]
    pub fn with_pcm(spec: AudioSpec, duration_secs: f64, bytes: &'static [u8]) -> Self {
        assert!(
            bytes.len().is_multiple_of(size_of::<f32>()),
            "prepared PCM must contain whole samples"
        );
        let reader = Self::build(spec, frames_for(spec, duration_secs), Source::Bytes(bytes));
        assert!(
            reader.total_frames <= bytes.len() / size_of::<f32>(),
            "prepared PCM is shorter than the requested track"
        );
        reader
    }

    fn build(spec: AudioSpec, total_frames: usize, source: Source) -> Self {
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

    fn sample_at(&self, start: usize, output_frame: usize) -> f32 {
        match &self.source {
            Source::Samples(samples) => samples[start + output_frame],
            Source::Bytes(bytes) => prepared_sample(bytes, start + output_frame),
        }
    }

    const fn at_natural_end(&self) -> bool {
        self.position_frames >= self.total_frames
    }

    /// Output frames still renderable before the source budget runs out.
    const fn output_frames_left(&self) -> usize {
        self.total_frames - self.position_frames
    }

    /// Advance the source cursor by `output_frames`, saturating at the total
    /// budget.
    fn consume(&mut self, output_frames: usize) {
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

    fn frames_to_duration(&self, frames: usize) -> Duration {
        self.spec
            .duration_for(frames as u64)
            .expect("mock frame count has a duration")
    }

    fn read_outcome(&self, written_frames: usize) -> ReadOutcome {
        let position = self.frames_to_duration(self.position_frames);
        NonZeroUsize::new(written_frames).map_or_else(
            || self.buffering(),
            |count| ReadOutcome::Frames {
                count,
                position,
                source_span: None,
            },
        )
    }

    fn buffering(&self) -> ReadOutcome {
        ReadOutcome::Pending {
            position: self.frames_to_duration(self.position_frames),
            reason: PendingReason::Buffering,
        }
    }
}

fn frames_for(spec: AudioSpec, duration_secs: f64) -> usize {
    spec.frames_for(Duration::from_secs_f64(duration_secs))
        .expect("mock duration has a frame count")
        .get()
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
        let channels = usize::from(self.spec.channels);
        if channels == 0 || buf.is_empty() {
            return Ok(self.buffering());
        }
        let to_write = buf.len().min(self.output_frames_left() * channels);
        let start = self.position_frames;
        for (index, sample) in buf[..to_write].iter_mut().enumerate() {
            *sample = self.sample_at(start, index / channels);
        }
        self.consume(to_write / channels);
        Ok(self.read_outcome(to_write))
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        if self.at_natural_end() {
            return Ok(self.eof_outcome());
        }
        let channels = usize::from(self.spec.channels);
        if output.is_empty() || channels == 0 || output.len() < channels {
            return Ok(self.buffering());
        }
        let frames_to_write = output[0].len().min(self.output_frames_left());
        let start = self.position_frames;
        for ch in output.iter_mut().take(channels) {
            for (frame, sample) in ch.iter_mut().take(frames_to_write).enumerate() {
                *sample = self.sample_at(start, frame);
            }
        }
        self.consume(frames_to_write);
        Ok(self.read_outcome(frames_to_write))
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }
}

impl AudioControl for TestPcmReader {
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        let target = position;
        let frame = self
            .spec
            .frames_for(position)
            .map_err(|source| DecodeError::Io {
                source: std::io::Error::other(source),
            })?
            .get();
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

/// Sample `frame` of prepared little-endian `f32` PCM.
pub(super) fn prepared_sample(bytes: &[u8], frame: usize) -> f32 {
    let index = frame * size_of::<f32>();
    f32::from_le_bytes(
        bytes[index..index + size_of::<f32>()]
            .try_into()
            .expect("prepared PCM sample"),
    )
}
