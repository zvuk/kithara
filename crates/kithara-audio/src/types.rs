use std::{num::NonZeroU32, time::Duration};

// Re-export for convenience
pub use kithara_decode::{DecodeError, DecodeResult};
use kithara_decode::{PcmSpec, TrackMetadata};

/// Primary PCM interface for reading decoded audio.
///
/// This is the main consumer-facing trait, replacing interleaved and planar
/// read patterns under a single interface.
pub trait PcmReader: Send {
    /// Read interleaved PCM samples into buffer.
    ///
    /// Returns the number of samples written (may be less than `buf.len()`).
    /// Returns 0 when EOF is reached.
    fn read(&mut self, buf: &mut [f32]) -> usize;

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// Each slice in `output` corresponds to one channel.
    /// Returns the number of frames written per channel.
    fn read_planar(&mut self, output: &mut [&mut [f32]]) -> usize {
        let channels = output.len();
        if channels == 0 {
            return 0;
        }
        let frames = output[0].len();
        let mut interleaved = vec![0.0f32; frames * channels];
        let n = self.read(&mut interleaved);
        let actual_frames = n / channels;
        for frame in 0..actual_frames {
            for (ch, out_ch) in output.iter_mut().enumerate() {
                out_ch[frame] = interleaved[frame * channels + ch];
            }
        }
        actual_frames
    }

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
}
