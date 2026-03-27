// unimock macro generates code triggering ignored_unit_patterns
#![allow(clippy::ignored_unit_patterns)]

//! Audio pipeline traits.

use std::{num::NonZeroU32, sync::Arc, time::Duration};

// Re-export for convenience
pub use kithara_decode::{DecodeError, DecodeResult};
use kithara_decode::{PcmChunk, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::tokio as platform_tokio;
use platform_tokio::sync::Notify;
#[cfg(any(test, feature = "test-utils"))]
use unimock::unimock;

use crate::ServiceClass;

/// Audio processing effect in the chain (transforms PCM chunks).
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = AudioEffectMock))]
pub trait AudioEffect: Send + 'static {
    /// Process a PCM chunk, returning transformed output.
    ///
    /// Returns `None` if the effect is accumulating data (not enough for output yet).
    fn process(&mut self, chunk: PcmChunk) -> Option<PcmChunk>;

    /// Flush remaining buffered data (called at end of stream).
    fn flush(&mut self) -> Option<PcmChunk>;

    /// Reset internal state (called after seek).
    fn reset(&mut self);
}

/// Primary PCM interface for reading decoded audio.
///
/// This is the main consumer-facing trait, replacing interleaved and planar
/// read patterns under a single interface.
///
/// **Usage pattern:**
/// ```ignore
/// // Async preload before audio callback
/// resource.preload().await;
///
/// // In audio callback (non-blocking after preload)
/// let frames = resource.read_planar(&mut buffers);
/// ```
#[cfg_attr(any(test, feature = "test-utils"), unimock(api = PcmReaderMock))]
pub trait PcmReader: Send {
    /// Read interleaved PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data without blocking.
    /// Returns the number of samples written (may be less than `buf.len()`).
    /// Returns 0 when no data is available or EOF is reached.
    fn read(&mut self, buf: &mut [f32]) -> usize;

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data without blocking.
    /// Each slice in `output` corresponds to one channel.
    /// Returns the number of frames written per channel.
    fn read_planar<'a>(&mut self, output: &'a mut [&'a mut [f32]]) -> usize;

    /// Seek to the given position.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] if the seek operation fails.
    fn seek(&mut self, position: Duration) -> DecodeResult<()>;

    /// Get the current PCM specification.
    fn spec(&self) -> PcmSpec;

    /// Check if end of stream has been reached.
    fn is_eof(&self) -> bool;

    /// Get current playback position.
    fn position(&self) -> Duration;

    /// Get total duration (if known).
    fn duration(&self) -> Option<Duration>;

    /// Get track metadata.
    fn metadata(&self) -> &TrackMetadata;

    /// Access the unified event bus for subscribing to all pipeline events.
    fn event_bus(&self) -> &EventBus;

    /// Set the target sample rate of the audio host.
    ///
    /// Used for dynamic updates when the host sample rate changes at runtime.
    fn set_host_sample_rate(&self, _sample_rate: NonZeroU32) {}

    /// Set the playback rate for timeline scaling.
    ///
    /// Rate > 1.0 speeds up playback (position advances faster).
    /// Rate < 1.0 slows down playback (position advances slower).
    /// The actual pitch-shifting is done by the resampler.
    fn set_playback_rate(&self, _rate: f32) {}

    /// Update the scheduling priority hint for the shared worker.
    ///
    /// Maps track playback state to worker priority: `Audible` tracks
    /// are decoded first, then `Warm`, then `Idle`.
    fn set_service_class(&self, _class: ServiceClass) {}
    /// Get notify for async preload (first chunk available).
    fn preload_notify(&self) -> Option<Arc<Notify>> {
        None
    }

    /// Preload initial chunks into internal buffers.
    ///
    /// After calling this, subsequent `read_available()` / `read_planar_available()`
    /// will return immediately from buffered data without blocking.
    fn preload(&mut self) {}

    /// Read the next decoded chunk with full metadata.
    ///
    /// Returns `None` on EOF or channel close.
    /// Discards any partially-consumed chunk from previous [`PcmReader::read`] calls.
    ///
    /// Use this for chunk-level inspection and stream integrity verification.
    fn next_chunk(&mut self) -> Option<PcmChunk> {
        None
    }
}
