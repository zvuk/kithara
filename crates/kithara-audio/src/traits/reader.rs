use std::num::NonZeroU32;

use kithara_decode::{DecodeError, PcmSpec, TrackMetadata};
use kithara_events::EventBus;
use kithara_platform::{maybe_send::MaybeSend, sync::Arc, time::Duration};

use super::{ChunkOutcome, PresentationAdvance, PresentationPoint, ReadOutcome, SeekOutcome};
use crate::{ServiceClass, renderer::PreloadGate};

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// PCM data-plane operations.
#[kithara::mock(api = PcmReadMock)]
pub trait PcmRead {
    /// Cached span: the timestamp up to which the source's bytes are on disk
    /// and need no further network. Unrelated to [`Self::position`] — bytes
    /// land ahead of the decoder. Readers with no download side report `0`.
    fn cached_span(&self) -> Duration {
        Duration::from_secs(0)
    }

    /// Decoded-ahead frontier: the timestamp up to which PCM has been
    /// decoded and is ready to play. Always `>=` [`Self::position`].
    /// Authoritative source for the buffered/playable window; non-adaptive
    /// or chunk-less readers may report `0`.
    fn decoded_frontier(&self) -> Duration {
        Duration::from_secs(0)
    }

    /// Latest coherent producer-side presentation endpoint.
    ///
    /// This may describe PCM still ahead of the consumer. Use
    /// [`Self::take_presentation_advance`] for proof of consumption.
    fn presentation_point(&self) -> Option<PresentationPoint> {
        None
    }

    /// Takes the latest exact final-block boundary crossed by PCM reads.
    ///
    /// Partial block consumption does not produce an advance. If one read
    /// crosses multiple boundaries, this returns the latest one and its exact
    /// frame offset within that read.
    fn take_presentation_advance(&mut self) -> Option<PresentationAdvance> {
        None
    }

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

    /// Get current playback position.
    fn position(&self) -> Duration;

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

    /// Get the current PCM specification.
    fn spec(&self) -> PcmSpec;
}

/// PCM track and session introspection.
#[kithara::mock(api = PcmSessionMock)]
pub trait PcmSession {
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

    /// Get track metadata.
    fn metadata(&self) -> &TrackMetadata;

    /// Decoder epoch whose preload gate should be observed by async callers.
    ///
    /// Readers without epoch-based seek invalidation keep the default initial
    /// epoch (`0`). Worker-backed [`Audio`](crate::audio::Audio) readers return
    /// the current seek epoch so a stale pre-seek signal cannot release a
    /// post-seek preload wait.
    fn preload_epoch(&self) -> u64 {
        0
    }

    /// Startup gate signalled once preload completes (first chunk
    /// available). The async consumer awaits [`PreloadGate::wait`]; the
    /// worker opens it with a lock-free store. `None` for readers without
    /// a worker-backed preload (file, test fixtures).
    fn preload_gate(&self) -> Option<Arc<PreloadGate>> {
        None
    }
}

/// The control-plane half of a seek: the part that publishes an event and wakes a decode worker,
/// both lock-taking, leaving the audio callback only [`PcmControl::sync_seek`].
pub trait SeekBegin: Send + Sync {
    /// Begin a seek to `position` and report where it will land. Blocking by design — never call
    /// this from an audio callback.
    fn begin(&self, position: Duration) -> SeekOutcome;
}

/// PCM control operations and runtime knobs.
#[kithara::mock(api = PcmControlMock)]
pub trait PcmControl {
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

    /// Control-plane handle that begins a seek without touching the reader, leaving
    /// [`sync_seek`](Self::sync_seek) to pick the target up.
    ///
    /// `None` means the reader cannot be seeked from an audio callback at all: such a caller must
    /// reach it off-thread through [`seek`](Self::seek).
    fn seek_handle(&self) -> Option<Arc<dyn SeekBegin>> {
        None
    }

    /// Adopt a seek epoch begun through [`seek_handle`](Self::seek_handle). Must be lock-free —
    /// this is the only half an audio callback may run.
    fn sync_seek(&mut self) {}

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
