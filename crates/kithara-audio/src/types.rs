use std::{num::NonZeroU32, sync::Arc, time::Duration};

// Re-export for convenience
pub use kithara_decode::{DecodeError, DecodeResult};
use kithara_decode::{PcmSpec, TrackMetadata};
use tokio::sync::Notify;

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
    fn read_planar(&mut self, output: &mut [&mut [f32]]) -> usize;

    /// Seek to the given position.
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

    /// Subscribe to audio events.
    fn decode_events(&self) -> tokio::sync::broadcast::Receiver<crate::AudioEvent>;

    /// Set the target sample rate of the audio host.
    ///
    /// Used for dynamic updates when the host sample rate changes at runtime.
    fn set_host_sample_rate(&self, _sample_rate: NonZeroU32) {}

    /// Get notify for async preload (first chunk available).
    fn preload_notify(&self) -> Option<Arc<Notify>> {
        None
    }

    /// Preload initial chunks into internal buffers.
    ///
    /// After calling this, subsequent `read_available()` / `read_planar_available()`
    /// will return immediately from buffered data without blocking.
    fn preload(&mut self) {}
}
