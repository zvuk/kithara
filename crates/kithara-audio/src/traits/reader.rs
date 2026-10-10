use std::num::NonZeroU32;

use kithara_decode::TrackMetadata;
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use super::{AudioReadError, ChunkOutcome, ReadOutcome, SeekOutcome};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Decoded-audio data-plane operations.
#[kithara::mock(api = AudioReadMock)]
pub trait AudioRead {
    /// Cached span: the timestamp up to which the source's bytes are on disk
    /// and need no further network. Unrelated to [`Self::position`] — bytes
    /// land ahead of the decoder. Readers with no download side report `0`.
    fn cached_span(&self) -> Duration {
        Duration::from_secs(0)
    }

    /// Decoded-ahead frontier: the timestamp up to which audio has been
    /// decoded and is ready to play. Always `>=` [`Self::position`].
    /// Authoritative source for the buffered/playable window; non-adaptive
    /// or chunk-less readers may report `0`.
    fn decoded_frontier(&self) -> Duration {
        Duration::from_secs(0)
    }

    /// Read a decoded chunk with metadata, yielding the unread tail of a partial [`AudioRead::read`].
    /// Returns a chunk or natural EOF; the default reports immediate EOF without chunk-level support.
    ///
    /// # Errors
    /// Returns terminal source failures with the same semantics as [`Self::read`].
    fn next_chunk(&mut self) -> Result<ChunkOutcome, AudioReadError> {
        Ok(ChunkOutcome::Eof {
            position: self.position(),
        })
    }

    /// Get current playback position.
    fn position(&self) -> Duration;

    /// Read interleaved samples, driving the source on its owning thread and copying available PCM.
    /// [`ReadOutcome`] distinguishes data from natural EOF; count is samples, or count/channels frames.
    ///
    /// # Errors
    /// Terminal source I/O, decoder or backend failures persist on subsequent reads.
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, AudioReadError>;

    /// Read deinterleaved (planar) audio samples.
    ///
    /// Drives the source on its owning thread. Each slice in `output` corresponds to one
    /// channel. The returned [`ReadOutcome`] has the same semantics as
    /// [`Self::read`]; `count` is frames-per-channel.
    ///
    /// # Errors
    ///
    /// Same as [`Self::read`] — terminal source failures are surfaced
    /// as `Err(AudioReadError)`.
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, AudioReadError>;

    /// Get the current decoded-audio specification.
    fn spec(&self) -> AudioSpec;
}

/// Decoded-audio track and session introspection.
#[kithara::mock(api = AudioSessionMock)]
pub trait AudioSession {
    /// Runtime ABR handle for the underlying stream.
    ///
    /// Adaptive readers (HLS) return `Some(handle)` so the queue/FFI can
    /// drive `set_mode` / `set_max_bandwidth_bps` mid-playback. Default
    /// `None` for non-adaptive readers (file, test fixtures).
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        None
    }

    /// Get total duration (if known).
    fn duration(&self) -> Option<Duration>;

    /// Access the unified event bus for subscribing to all pipeline events.
    fn event_bus(&self) -> &EventBus;

    /// Whether initial preparation was requested for this reader.
    fn is_preloaded(&self) -> bool {
        false
    }

    /// Get track metadata.
    fn metadata(&self) -> &TrackMetadata;
}

/// Decoded-audio control operations and runtime knobs.
#[kithara::mock(api = AudioControlMock)]
pub trait AudioControl {
    /// Prepare initial input on the chain's owning thread.
    /// Pending input and natural EOF do not constitute setup failures.
    ///
    /// # Errors
    /// Returns a source or decoder failure.
    fn preload(&mut self) -> Result<(), AudioReadError> {
        Ok(())
    }

    /// Seek to the given position.
    ///
    /// Returns [`SeekOutcome::Landed`] when the reader is now parked
    /// at the requested position, [`SeekOutcome::PastEof`] when the
    /// target was beyond `duration()`. The search is synchronous and never
    /// rebuilds a decoder merely because seeking failed.
    ///
    /// # Errors
    ///
    /// Returns a source, format-boundary rebuild, or decoder-search failure.
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, AudioReadError>;

    /// Set the target sample rate of the audio host.
    ///
    /// Used for dynamic updates when the host sample rate changes at runtime.
    fn set_host_sample_rate(&mut self, _sample_rate: NonZeroU32) {}
}

/// Primary interface for reading and controlling decoded audio.
///
/// **Terminal-state contract.** Three failure-mode-agnostic outcomes
/// are distinguishable by the caller:
///
/// - `Ok(ReadOutcome::Frames { .. })` — reader is alive and produced frames.
/// - `Ok(ReadOutcome::Eof { .. })` — natural end of stream.
/// - `Err(AudioReadError)` — decoder or channel failure.
pub trait AudioReader: AudioRead + AudioSession + AudioControl + Send {}

impl<T> AudioReader for T where T: AudioRead + AudioSession + AudioControl + Send {}
