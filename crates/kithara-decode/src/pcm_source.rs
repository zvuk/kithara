//! PcmSource: Source<Item=f32> implementation over decoded audio.
//!
//! Provides random-access to decoded PCM samples via the Source trait.

use std::{ops::Range, time::Duration};

use async_trait::async_trait;
use kithara_storage::WaitOutcome;
use kithara_stream::{Source, StreamError, StreamResult};

use crate::{traits::PcmBufferTrait, DecodeError, PcmSpec};

/// PCM audio source implementing `Source<Item=f32>`.
///
/// Provides random-access to decoded audio samples. Offsets and lengths
/// are in samples (not frames or bytes).
///
/// Generic over buffer implementation for testability.
///
/// ## Usage
///
/// ```ignore
/// // Create unified pipeline
/// let pipeline = UnifiedPipeline::open(byte_source, 44100).await?;
///
/// // Create PCM source
/// let pcm_source = PcmSource::new(pipeline.buffer().clone(), pipeline.output_spec());
///
/// // Read samples
/// let mut buf = vec![0.0f32; 1024];
/// pcm_source.read_at(sample_offset, &mut buf).await?;
/// ```
pub struct PcmSource<B> {
    /// Shared PCM buffer.
    buffer: B,
    /// Output specification.
    spec: PcmSpec,
}

impl<B: PcmBufferTrait> PcmSource<B> {
    /// Create a new PCM source from a buffer.
    pub fn new(buffer: B, spec: PcmSpec) -> Self {
        Self { buffer, spec }
    }

    /// Get PCM specification.
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Get total samples available (samples, not frames).
    pub fn samples_available(&self) -> u64 {
        let frames = self.buffer.frames_written();
        frames * self.spec.channels as u64
    }

    /// Check if EOF has been reached.
    pub fn is_eof(&self) -> bool {
        self.buffer.is_eof()
    }
}

#[async_trait]
impl<B: PcmBufferTrait + 'static> Source for PcmSource<B> {
    type Item = f32;
    type Error = DecodeError;

    async fn wait_range(&self, range: Range<u64>) -> StreamResult<WaitOutcome, Self::Error> {
        let channels = self.spec.channels as u64;

        // Convert sample range to frame range
        let frame_start = range.start / channels;
        let frame_end = (range.end + channels - 1) / channels; // round up

        // Poll with exponential backoff
        let mut wait_ms = 1;
        const MAX_WAIT_MS: u64 = 100;
        const MAX_ATTEMPTS: u32 = 1000; // ~50 seconds total

        for attempt in 0..MAX_ATTEMPTS {
            let frames_available = self.buffer.frames_written();

            // Check if range is available
            if frames_available >= frame_end {
                return Ok(WaitOutcome::Ready);
            }

            // Check if EOF reached
            if self.buffer.is_eof() {
                // If we have some data but not enough, return Ready
                if frames_available > frame_start {
                    return Ok(WaitOutcome::Ready);
                }
                // No data at all in this range
                return Ok(WaitOutcome::Eof);
            }

            // Wait before retry
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(wait_ms)).await;
                wait_ms = (wait_ms * 2).min(MAX_WAIT_MS);
            }
        }

        // Timeout
        Err(StreamError::Source(DecodeError::Io(
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout waiting for PCM data",
            ),
        )))
    }

    async fn read_at(&self, offset: u64, buf: &mut [Self::Item]) -> StreamResult<usize, Self::Error> {
        let channels = self.spec.channels as u64;
        let frame_offset = offset / channels;

        // Read samples from buffer
        let n = self.buffer.read_samples(frame_offset, buf);
        Ok(n)
    }

    fn len(&self) -> Option<u64> {
        // For streaming sources, length is unknown until EOF
        if self.buffer.is_eof() {
            Some(self.samples_available())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MockPcmBufferTrait;
    use rstest::*;
    use std::time::Duration;

    // Helper to create PcmSpec for tests
    fn test_spec(channels: u16, sample_rate: u32) -> PcmSpec {
        PcmSpec {
            sample_rate,
            channels,
        }
    }

    #[rstest]
    #[case(1, 44100)]  // Mono
    #[case(2, 48000)]  // Stereo
    #[case(6, 44100)]  // 5.1 surround
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_pcm_source_spec(#[case] channels: u16, #[case] sample_rate: u32) {
        let spec = test_spec(channels, sample_rate);
        let mut mock_buffer = MockPcmBufferTrait::new();

        mock_buffer.expect_spec().return_const(spec);

        let source = PcmSource::new(mock_buffer, spec);

        assert_eq!(source.spec(), spec);
    }

    #[rstest]
    #[case(100, 2, 200)]  // 100 frames * 2 channels = 200 samples
    #[case(1000, 1, 1000)]  // 1000 frames * 1 channel = 1000 samples
    #[case(500, 6, 3000)]  // 500 frames * 6 channels = 3000 samples
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_samples_available(
        #[case] frames: u64,
        #[case] channels: u16,
        #[case] expected_samples: u64,
    ) {
        let spec = test_spec(channels, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        mock_buffer.expect_frames_written().return_const(frames);

        let source = PcmSource::new(mock_buffer, spec);

        assert_eq!(source.samples_available(), expected_samples);
    }

    #[rstest]
    #[case(true)]
    #[case(false)]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_is_eof(#[case] eof_state: bool) {
        let spec = test_spec(2, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        mock_buffer.expect_is_eof().return_const(eof_state);

        let source = PcmSource::new(mock_buffer, spec);

        assert_eq!(source.is_eof(), eof_state);
    }

    #[rstest]
    #[case(0, 100, 2, 100)]  // Read 100 samples from offset 0, stereo
    #[case(50, 50, 1, 50)]   // Read 50 samples from offset 50, mono
    #[case(200, 200, 2, 200)] // Read 200 samples from offset 200, stereo
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_read_at_success(
        #[case] offset: u64,
        #[case] buf_size: usize,
        #[case] channels: u16,
        #[case] expected_read: usize,
    ) {
        let spec = test_spec(channels, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        // Mock read_samples to return expected_read
        mock_buffer
            .expect_read_samples()
            .times(1)
            .withf(move |frame_offset, buf| {
                *frame_offset == offset / channels as u64 && buf.len() == buf_size
            })
            .returning(move |_, buf| {
                // Fill buffer with test data
                for i in 0..expected_read.min(buf.len()) {
                    buf[i] = i as f32 * 0.1;
                }
                expected_read
            });

        let source = PcmSource::new(mock_buffer, spec);

        let mut buf = vec![0.0f32; buf_size];
        let n = source.read_at(offset, &mut buf).await.unwrap();

        assert_eq!(n, expected_read);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_read_at_empty_buffer() {
        let spec = test_spec(2, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        mock_buffer
            .expect_read_samples()
            .times(1)
            .returning(|_, _| 0);  // No data available

        let source = PcmSource::new(mock_buffer, spec);

        let mut buf = vec![0.0f32; 100];
        let n = source.read_at(0, &mut buf).await.unwrap();

        assert_eq!(n, 0);
    }

    #[rstest]
    #[case(Some(1000))]  // EOF reached, known length
    #[case(None)]        // EOF not reached, unknown length
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_len(#[case] expected_len: Option<u64>) {
        let spec = test_spec(2, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        let is_eof = expected_len.is_some();
        mock_buffer.expect_is_eof().return_const(is_eof);

        if is_eof {
            let frames = expected_len.unwrap() / 2;  // samples / channels
            mock_buffer.expect_frames_written().return_const(frames);
        }

        let source = PcmSource::new(mock_buffer, spec);

        assert_eq!(source.len(), expected_len);
    }

    #[rstest]
    #[case(0, 100, 2, 50, true)]  // Range 0..100 samples, 50 frames available, stereo -> ready
    #[case(0, 200, 2, 50, false)] // Range 0..200 samples, only 50 frames, stereo -> wait
    #[case(0, 100, 1, 100, true)] // Range 0..100 samples, 100 frames, mono -> ready
    #[timeout(Duration::from_secs(2))]
    #[tokio::test]
    async fn test_wait_range_scenarios(
        #[case] start: u64,
        #[case] end: u64,
        #[case] channels: u16,
        #[case] frames_available: u64,
        #[case] should_be_ready: bool,
    ) {
        let spec = test_spec(channels, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        // First call to check frames
        mock_buffer
            .expect_frames_written()
            .return_const(frames_available);

        if !should_be_ready {
            // If not ready, also need is_eof check
            mock_buffer.expect_is_eof().return_const(false);
        }

        let source = PcmSource::new(mock_buffer, spec);

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            source.wait_range(start..end)
        ).await;

        if should_be_ready {
            assert!(result.is_ok());
            assert_eq!(result.unwrap().unwrap(), WaitOutcome::Ready);
        } else {
            // Should timeout because data not available
            assert!(result.is_err());
        }
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_wait_range_eof_with_data() {
        let spec = test_spec(2, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        // Less data than requested, but EOF reached
        mock_buffer.expect_frames_written().return_const(50u64);
        mock_buffer.expect_is_eof().return_const(true);

        let source = PcmSource::new(mock_buffer, spec);

        // Request 200 samples (100 frames), but only 50 frames available
        let result = source.wait_range(0..200).await.unwrap();

        // Should return Ready because we have SOME data and EOF
        assert_eq!(result, WaitOutcome::Ready);
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_wait_range_eof_no_data() {
        let spec = test_spec(2, 44100);
        let mut mock_buffer = MockPcmBufferTrait::new();

        // No data and EOF
        mock_buffer.expect_frames_written().return_const(0u64);
        mock_buffer.expect_is_eof().return_const(true);

        let source = PcmSource::new(mock_buffer, spec);

        // Request from offset 100
        let result = source.wait_range(100..200).await.unwrap();

        // Should return Eof because no data in requested range
        assert_eq!(result, WaitOutcome::Eof);
    }
}
