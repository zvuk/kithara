use std::num::NonZeroU32;

use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{maybe_send::MaybeSend, sync::Arc, time::Duration};

use super::{ChunkOutcome, ReadOutcome, SeekOutcome};
use crate::{
    ServiceClass, SourceRange, SourceRangeError, SourceRangeReadOutcome, SourceRangeRequest,
    renderer::PreloadGate,
};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// PCM data-plane operations.
#[kithara::mock(api = PcmReadMock)]
pub trait PcmRead {
    /// Read interleaved PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data
    /// without blocking. The returned [`ReadOutcome`] distinguishes
    /// "produced N frames" (including `count == 0` for transient
    /// stalls) from natural EOF. Decoder / channel failures surface as
    /// `Err(DecodeError)`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` for terminal producer failures:
    /// closed PCM channel, decoder fault, or backend error. The error
    /// is one-way — once returned, subsequent reads continue to fail.
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError>;

    /// Read deinterleaved (planar) PCM samples.
    ///
    /// After `preload()`, returns immediately from buffered data
    /// without blocking. Each slice in `output` corresponds to one
    /// channel. The returned [`ReadOutcome`] has the same semantics as
    /// [`Self::read`]; `count` is frames-per-channel.
    ///
    /// # Errors
    ///
    /// Same as [`Self::read`] — terminal producer failures are surfaced
    /// as `Err(DecodeError)`.
    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError>;

    /// Read the next decoded chunk with full metadata.
    ///
    /// Returns [`ChunkOutcome::Chunk`] or [`ChunkOutcome::Eof`].
    /// Decoder / channel failures surface as `Err(DecodeError)`.
    /// Discards any partially-consumed chunk from previous
    /// [`PcmRead::read`] calls.
    ///
    /// Default implementation reports immediate natural EOF — readers
    /// without chunk-level support shouldn't be polled this way.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` for terminal producer failures, same
    /// semantics as [`Self::read`].
    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        Ok(ChunkOutcome::Eof {
            position: self.position(),
        })
    }

    /// Get the current PCM specification.
    fn spec(&self) -> PcmSpec;

    /// Get current playback position.
    fn position(&self) -> Duration;

    /// Decoded-ahead frontier: the timestamp up to which PCM has been
    /// decoded and is ready to play. Always `>=` [`Self::position`].
    /// Authoritative source for the buffered/playable window; non-adaptive
    /// or chunk-less readers may report `0`.
    fn decoded_frontier(&self) -> Duration {
        Duration::from_secs(0)
    }

    /// Poll a previously requested bounded range from the canonical PCM ring.
    ///
    /// The destination is interleaved and must hold exactly
    /// `request.range().len() * spec().channels` samples. Implementations that
    /// do not expose decoded source coordinates return
    /// [`SourceRangeError::Unsupported`].
    ///
    /// # Errors
    ///
    /// Returns a typed range error for stale requests, shape mismatches,
    /// discontinuities, or terminal decode failure.
    fn read_source_range(
        &mut self,
        _request: SourceRangeRequest,
        _output: &mut [f32],
    ) -> Result<SourceRangeReadOutcome, SourceRangeError> {
        Err(SourceRangeError::Unsupported)
    }
}

/// PCM track and session introspection.
#[kithara::mock(api = PcmSessionMock)]
pub trait PcmSession {
    /// Get track metadata.
    fn metadata(&self) -> &TrackMetadata;

    /// Get total duration (if known).
    fn duration(&self) -> Option<Duration>;

    /// Access the unified event bus for subscribing to all pipeline events.
    fn event_bus(&self) -> &EventBus;

    /// Runtime ABR handle for the underlying stream.
    ///
    /// Adaptive readers (HLS) return `Some(handle)` so the queue/FFI can
    /// drive `set_mode` / `set_max_bandwidth_bps` mid-playback. Default
    /// `None` for non-adaptive readers (file, test fixtures).
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        None
    }

    /// Startup gate signalled once preload completes (first chunk
    /// available). The async consumer awaits [`PreloadGate::wait`]; the
    /// worker opens it with a lock-free store. `None` for readers without
    /// a worker-backed preload (file, test fixtures).
    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        None
    }

    /// Decoder epoch whose preload gate should be observed by async callers.
    ///
    /// Readers without epoch-based seek invalidation keep the default initial
    /// epoch (`0`). Worker-backed [`Audio`](crate::audio::Audio) readers return
    /// the current seek epoch so a stale pre-seek signal cannot release a
    /// post-seek preload wait.
    fn preload_epoch(&self) -> u64 {
        0
    }
}

/// PCM control operations and runtime knobs.
#[kithara::mock(api = PcmControlMock)]
pub trait PcmControl {
    /// Seek to the given position.
    ///
    /// Returns [`SeekOutcome::Landed`] when the reader is now parked
    /// at the requested position, [`SeekOutcome::PastEof`] when the
    /// target was beyond `duration()`. Seek failures (stream I/O,
    /// decoder recreate) surface as `Err(DecodeError)`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` when seek cannot complete: stream I/O
    /// failure, decoder recreate failure, or terminal producer error.
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError>;

    /// Preload initial chunks into internal buffers.
    ///
    /// After calling this, subsequent `read()` / `read_planar()` /
    /// `next_chunk()` return immediately from buffered data without
    /// blocking. `Err(DecodeError)` is reserved for setup failures
    /// (e.g. the producer channel closed during preload). Natural EOF
    /// encountered during preload is **not** surfaced here — the
    /// subsequent `read` / `next_chunk` will return `Eof`.
    ///
    /// # Errors
    ///
    /// Returns `Err(DecodeError)` only on terminal setup failure
    /// (closed PCM channel, backend error). Successful preload always
    /// returns `Ok(())` even if the stream contains no data.
    fn preload(&mut self) -> Result<(), DecodeError> {
        Ok(())
    }

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

    /// Set the transport pitch-bend multiplier.
    ///
    /// `1.0` leaves source-frame consumption unchanged. Values above or
    /// below unity speed up or slow down the transport reader.
    fn set_transport_bend(&self, _bend: f32) {}

    /// Update the scheduling priority hint for the shared worker.
    ///
    /// Maps track playback state to worker priority: `Audible` tracks
    /// are decoded first, then `Warm`, then `Idle`.
    fn set_service_class(&self, _class: ServiceClass) {}

    /// Starts a bounded read on the canonical decoded source axis.
    ///
    /// The request owns the seek epoch created for the range. Starting another
    /// seek or range invalidates it. Readers without this capability return
    /// [`SourceRangeError::Unsupported`].
    ///
    /// # Errors
    ///
    /// Returns a typed range or decode error when the range cannot be started.
    fn request_source_range(
        &mut self,
        _range: SourceRange,
    ) -> Result<SourceRangeRequest, SourceRangeError> {
        Err(SourceRangeError::Unsupported)
    }
}

/// Primary PCM interface for reading and controlling decoded audio.
///
/// **Terminal-state contract.** Three failure-mode-agnostic outcomes
/// are distinguishable by the caller:
///
/// - `Ok(ReadOutcome::Frames { .. })` — reader is alive and produced frames.
/// - `Ok(ReadOutcome::Eof { .. })` — natural end of stream.
/// - `Err(DecodeError)` — decoder or channel failure.
pub trait PcmReader: PcmRead + PcmSession + PcmControl + MaybeSend {}

impl<T> PcmReader for T where T: PcmRead + PcmSession + PcmControl + MaybeSend {}

impl<T> PcmRead for Box<T>
where
    T: PcmReader + ?Sized,
{
    fn read(&mut self, buf: &mut [f32]) -> Result<ReadOutcome, DecodeError> {
        (**self).read(buf)
    }

    fn read_planar<'a>(
        &mut self,
        output: &'a mut [&'a mut [f32]],
    ) -> Result<ReadOutcome, DecodeError> {
        (**self).read_planar(output)
    }

    fn next_chunk(&mut self) -> Result<ChunkOutcome, DecodeError> {
        (**self).next_chunk()
    }

    fn spec(&self) -> PcmSpec {
        (**self).spec()
    }

    fn position(&self) -> Duration {
        (**self).position()
    }

    fn decoded_frontier(&self) -> Duration {
        (**self).decoded_frontier()
    }

    fn read_source_range(
        &mut self,
        request: SourceRangeRequest,
        output: &mut [f32],
    ) -> Result<SourceRangeReadOutcome, SourceRangeError> {
        (**self).read_source_range(request, output)
    }
}

impl<T> PcmSession for Box<T>
where
    T: PcmReader + ?Sized,
{
    fn metadata(&self) -> &TrackMetadata {
        (**self).metadata()
    }

    fn duration(&self) -> Option<Duration> {
        (**self).duration()
    }

    fn event_bus(&self) -> &EventBus {
        (**self).event_bus()
    }

    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        (**self).abr_handle()
    }

    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        (**self).preload_gate()
    }

    fn preload_epoch(&self) -> u64 {
        (**self).preload_epoch()
    }
}

impl<T> PcmControl for Box<T>
where
    T: PcmReader + ?Sized,
{
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, DecodeError> {
        (**self).seek(position)
    }

    fn preload(&mut self) -> Result<(), DecodeError> {
        (**self).preload()
    }

    fn set_host_sample_rate(&self, sample_rate: NonZeroU32) {
        (**self).set_host_sample_rate(sample_rate);
    }

    fn set_playback_rate(&self, rate: f32) {
        (**self).set_playback_rate(rate);
    }

    fn set_transport_bend(&self, bend: f32) {
        (**self).set_transport_bend(bend);
    }

    fn set_service_class(&self, class: ServiceClass) {
        (**self).set_service_class(class);
    }

    fn request_source_range(
        &mut self,
        range: SourceRange,
    ) -> Result<SourceRangeRequest, SourceRangeError> {
        (**self).request_source_range(range)
    }
}
