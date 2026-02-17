//! RT-safe wrapper around [`Resource`] for audio-thread access.
//!
//! [`PlayerResource`] adds internal PCM scratch buffers so that the audio
//! callback can read planar PCM data without allocating. Access from the
//! audio thread is always via `Arc<kithara_platform::Mutex<PlayerResource>>`
//! with `try_lock()`.

use std::{num::NonZeroU32, ops::Range, sync::Arc, time::Duration};

use kithara_bufpool::{PcmBuf, pcm_pool};
use tracing::warn;

use crate::{error::PlayError, impls::resource::Resource};

/// RT-safe resource wrapper with internal scratch buffers.
///
/// Wraps a [`Resource`] and maintains per-channel scratch buffers
/// that are filled from the underlying `PcmReader`. The audio thread
/// reads from these buffers, avoiding direct interaction with the
/// potentially-blocking decoder on every callback.
pub(crate) struct PlayerResource {
    resource: Resource,
    channel_buffers: [PcmBuf; 2],
    write_len: usize,
    write_pos: usize,
    src: Arc<str>,
}

impl PlayerResource {
    /// Create a new `PlayerResource` wrapping the given resource.
    ///
    /// Allocates two channel scratch buffers from the global PCM pool,
    /// sized to `sample_rate / 5` frames (200ms worth of audio).
    pub(crate) fn new(resource: Resource, src: Arc<str>) -> Self {
        let spec = resource.spec();
        let channels = spec.channels as usize;
        let buffer_len = (spec.sample_rate as usize / 5) * channels.max(2);

        let channel_buffers = [
            pcm_pool().get_with(|b: &mut Vec<f32>| {
                let cap = b.capacity();
                if cap < buffer_len {
                    b.reserve(buffer_len - cap);
                }
                b.resize(buffer_len, 0.0);
            }),
            pcm_pool().get_with(|b: &mut Vec<f32>| {
                let cap = b.capacity();
                if cap < buffer_len {
                    b.reserve(buffer_len - cap);
                }
                b.resize(buffer_len, 0.0);
            }),
        ];

        Self {
            resource,
            channel_buffers,
            write_len: 0,
            write_pos: 0,
            src,
        }
    }

    /// Read PCM frames into the output buffers for the given range.
    ///
    /// Fills internal scratch buffers from the underlying resource as needed,
    /// then copies the requested frames into `output`. Shifts any remaining
    /// data to the front of the scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns `PlayError::Internal` with "eof" if the resource has reached
    /// end of file and no buffered data remains.
    pub(crate) fn read(
        &mut self,
        output: &mut [&mut [f32]],
        range: Range<usize>,
    ) -> Result<(), PlayError> {
        let frames_to_read = range.end - range.start;
        let mut eof_reached = false;

        // Fill scratch buffers until we have enough data
        while frames_to_read > self.write_len {
            if self.resource.is_eof() {
                eof_reached = true;
                break;
            }

            let avail = self.channel_buffers[0].len() - self.write_pos;
            if avail == 0 {
                break;
            }

            let channel_buffers = &mut self.channel_buffers;
            let (left_buf, right_buf) = channel_buffers.split_at_mut(1);
            let left = &mut left_buf[0][self.write_pos..self.write_pos + avail];
            let right = &mut right_buf[0][self.write_pos..self.write_pos + avail];
            let mut planar: [&mut [f32]; 2] = [left, right];

            let n = self.resource.read_planar(&mut planar);
            if n == 0 {
                eof_reached = true;
                break;
            }
            self.write_len += n;
            self.write_pos += n;
        }

        // Copy from scratch buffers to output
        if self.write_len > 0 {
            let frames_to_write = frames_to_read.min(self.write_len);
            let tail_size = self.write_len - frames_to_write;

            if output.len() >= 2 {
                output[0][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[0][..frames_to_write]);
                output[1][..frames_to_write]
                    .copy_from_slice(&self.channel_buffers[1][..frames_to_write]);

                // Zero-fill any unfilled portion of the requested range to avoid
                // stale data leaking into the output when a partial read occurs
                // (e.g. near EOF).
                let range_len = range.len();
                if frames_to_write < range_len {
                    for ch in output.iter_mut() {
                        ch[frames_to_write..range_len].fill(0.0);
                    }
                }
            }

            // Shift remaining data to front
            if tail_size > 0 {
                self.channel_buffers[0]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
                self.channel_buffers[1]
                    .copy_within(frames_to_write..frames_to_write + tail_size, 0);
            }

            self.write_len -= frames_to_write;
            self.write_pos = tail_size;

            Ok(())
        } else if eof_reached {
            Err(PlayError::Internal("eof".into()))
        } else {
            Ok(())
        }
    }

    /// Seek to the given position in seconds.
    ///
    /// Clears the internal scratch buffers on success.
    pub(crate) fn seek(&mut self, seconds: f64) {
        let position = Duration::from_secs_f64(seconds);
        match self.resource.seek(position) {
            Ok(()) => {
                self.write_len = 0;
                self.write_pos = 0;
            }
            Err(err) => {
                warn!("failed to seek: {err}");
            }
        }
    }

    /// Current playback position in seconds.
    pub(crate) fn position(&self) -> f64 {
        self.resource.position().as_secs_f64()
    }

    /// Total duration in seconds. Returns 0.0 if unknown.
    pub(crate) fn duration(&self) -> f64 {
        self.resource.duration().map_or(0.0, |d| d.as_secs_f64())
    }

    /// Source identifier for this resource.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by Task 9 wiring"))]
    pub(crate) fn src(&self) -> &Arc<str> {
        &self.src
    }

    /// Set the target sample rate of the audio host.
    pub(crate) fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        self.resource.set_host_sample_rate(sample_rate);
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    reason = "test mock code; values are small and positive by construction"
)]
mod tests {
    use std::time::Duration;

    use kithara_audio::PcmReader;
    use kithara_decode::{DecodeResult, PcmSpec, TrackMetadata};
    use kithara_events::AudioEvent;
    use tokio::sync::broadcast;

    use super::*;

    /// Mock `PcmReader` for testing `PlayerResource`.
    struct MockPcmReader {
        spec: PcmSpec,
        metadata: TrackMetadata,
        total_frames: u64,
        position_frames: u64,
        eof: bool,
        sample_value: f32,
        events_tx: broadcast::Sender<AudioEvent>,
    }

    impl MockPcmReader {
        fn new(spec: PcmSpec, duration_secs: f64, sample_value: f32) -> Self {
            let total_frames = (spec.sample_rate as f64 * duration_secs) as u64;
            let (events_tx, _) = broadcast::channel(64);
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
                sample_value,
                events_tx,
            }
        }

        fn frames_to_duration(&self, frames: u64) -> Duration {
            if self.spec.sample_rate == 0 {
                return Duration::ZERO;
            }
            Duration::from_secs_f64(frames as f64 / self.spec.sample_rate as f64)
        }
    }

    impl PcmReader for MockPcmReader {
        fn read(&mut self, buf: &mut [f32]) -> usize {
            if self.eof {
                return 0;
            }
            let channels = self.spec.channels as u64;
            if channels == 0 {
                return 0;
            }
            let remaining_samples = (self.total_frames - self.position_frames) * channels;
            let to_write = (buf.len() as u64).min(remaining_samples) as usize;
            for sample in &mut buf[..to_write] {
                *sample = self.sample_value;
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
            let channels = self.spec.channels as usize;
            if channels == 0 || output.len() < channels {
                return 0;
            }
            let frames_per_channel = output[0].len();
            let remaining = (self.total_frames - self.position_frames) as usize;
            let frames_to_write = frames_per_channel.min(remaining);
            for ch in output.iter_mut().take(channels) {
                for sample in ch.iter_mut().take(frames_to_write) {
                    *sample = self.sample_value;
                }
            }
            self.position_frames += frames_to_write as u64;
            if self.position_frames >= self.total_frames {
                self.eof = true;
            }
            frames_to_write
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<()> {
            let frame = (position.as_secs_f64() * self.spec.sample_rate as f64) as u64;
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

        fn decode_events(&self) -> broadcast::Receiver<AudioEvent> {
            self.events_tx.subscribe()
        }
    }

    fn mock_spec() -> PcmSpec {
        PcmSpec {
            channels: 2,
            sample_rate: 44100,
        }
    }

    fn make_player_resource() -> PlayerResource {
        let reader = MockPcmReader::new(mock_spec(), 1.0, 0.5);
        let resource = Resource::from_reader(reader);
        PlayerResource::new(resource, Arc::from("test.mp3"))
    }

    #[tokio::test]
    async fn resource_new_creates_buffers() {
        let pr = make_player_resource();
        // Buffer size = (44100 / 5) * max(2, 2) = 8820 * 2 = 17640
        assert!(!pr.channel_buffers[0].is_empty());
        assert!(!pr.channel_buffers[1].is_empty());
        assert_eq!(pr.write_len, 0);
        assert_eq!(pr.write_pos, 0);
    }

    #[tokio::test]
    async fn resource_read_returns_samples() {
        let mut pr = make_player_resource();
        let mut left = vec![0.0f32; 128];
        let mut right = vec![0.0f32; 128];
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let result = pr.read(&mut output, 0..128);
        assert!(result.is_ok());
        // Should have filled with 0.5 sample value
        for &s in &left[..128] {
            assert!((s - 0.5).abs() < f32::EPSILON);
        }
        for &s in &right[..128] {
            assert!((s - 0.5).abs() < f32::EPSILON);
        }
    }

    #[tokio::test]
    async fn resource_seek_clears_buffer() {
        let mut pr = make_player_resource();

        // Read some data first to fill buffers
        let mut left = vec![0.0f32; 128];
        let mut right = vec![0.0f32; 128];
        let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
        let _ = pr.read(&mut output, 0..128);

        // Seek resets internal buffer state
        pr.seek(0.5);
        assert_eq!(pr.write_len, 0);
        assert_eq!(pr.write_pos, 0);
    }

    #[tokio::test]
    async fn resource_eof_returns_error() {
        let reader = MockPcmReader::new(mock_spec(), 0.01, 0.5);
        let resource = Resource::from_reader(reader);
        let mut pr = PlayerResource::new(resource, Arc::from("short.mp3"));

        // Read all data
        let mut left = vec![0.0f32; 4096];
        let mut right = vec![0.0f32; 4096];
        loop {
            let mut output: Vec<&mut [f32]> = vec![&mut left, &mut right];
            match pr.read(&mut output, 0..4096) {
                Ok(()) => {
                    if pr.write_len == 0 && pr.resource.is_eof() {
                        // Next read should return eof error
                        let mut output2: Vec<&mut [f32]> = vec![&mut left, &mut right];
                        let result = pr.read(&mut output2, 0..4096);
                        assert!(result.is_err());
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    #[tokio::test]
    async fn resource_position_and_duration() {
        let pr = make_player_resource();
        assert_eq!(pr.position(), 0.0);
        assert!((pr.duration() - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn resource_src() {
        let pr = make_player_resource();
        assert_eq!(&**pr.src(), "test.mp3");
    }
}
