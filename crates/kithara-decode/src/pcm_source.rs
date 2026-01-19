//! PcmSource: Source<Item=f32> implementation over decoded audio.
//!
//! Provides random-access to decoded PCM samples via the Source trait.

use std::{ops::Range, sync::Arc, time::Duration};

use async_trait::async_trait;
use kithara_storage::WaitOutcome;
use kithara_stream::{Source, StreamError, StreamResult};

use crate::{DecodeError, PcmBuffer, PcmSpec};

/// PCM audio source implementing `Source<Item=f32>`.
///
/// Provides random-access to decoded audio samples. Offsets and lengths
/// are in samples (not frames or bytes).
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
pub struct PcmSource {
    /// Shared PCM buffer.
    buffer: Arc<PcmBuffer>,
    /// Output specification.
    spec: PcmSpec,
}

impl PcmSource {
    /// Create a new PCM source from a buffer.
    pub fn new(buffer: Arc<PcmBuffer>, spec: PcmSpec) -> Self {
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
impl Source for PcmSource {
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
