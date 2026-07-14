use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use delegate::delegate;
use kithara_platform::sync::{Arc, Mutex};
use kithara_stream::{
    Activity, ByteMap, MediaInfo, PlayheadWrite, SeekControl, SeekObserve, SourcePhase,
    SourceSeekAnchor, Stream, StreamType, WorkerWake,
};

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
pub(crate) struct SharedStream<T: StreamType> {
    /// Construction-phase read mode, shared across clones. When set, `Read`
    /// routes through the blocking off-RT [`Stream::read`] adapter (waits for
    /// the seeked range to download, cancel-bounded by its own timeout)
    /// instead of the non-blocking RT [`Stream::probe_read`]. `Audio::new`
    /// arms it for the single up-front decoder build (with the init body
    /// prefetched, the build read normally hits committed bytes; the blocking
    /// adapter is the bounded safety net for residual lateness) and disarms it
    /// before the worker is registered, so the RT decode loop the worker then
    /// drives always uses `probe_read`. See the crate `CONTEXT.md`
    /// "Construction reads".
    blocking: Arc<AtomicBool>,
    inner: Arc<Mutex<Stream<T>>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
            blocking: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Arm/disarm the construction-phase blocking read mode (see the
    /// `blocking` field). Shared across all clones derived from this handle,
    /// so arming before the decoder build and disarming before worker
    /// registration is a staged construction → steady-state ownership
    /// transfer — never a live toggle once the RT worker runs.
    pub(crate) fn set_blocking(&self, on: bool) {
        self.blocking.store(on, Ordering::Release);
    }

    delegate! {
        to self.inner.lock() {
            pub(crate) fn position(&self) -> u64;
            /// Absolute byte cursor set — forwards to the inner source's
            /// atomic, used post-seek when the audio FSM lands at a
            /// known byte position.
            pub(crate) fn set_position(&self, pos: u64);
            pub(crate) fn len(&self) -> Option<u64>;
            pub(crate) fn media_info(&self) -> Option<MediaInfo>;
            pub(crate) fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            pub(crate) fn format_change_segment_range(&self) -> kithara_stream::StreamResult<Range<u64>>;
            pub(crate) fn clear_variant_fence(&self);
            pub(crate) fn has_variant_change_pending(&self) -> bool;
            pub(crate) fn variant_change_target(&self) -> Option<usize>;
            pub(crate) fn seek_time_anchor(&self, position: kithara_platform::time::Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            /// Build a fresh reader-side event-sink instance from the inner source.
            pub(crate) fn take_reader_event_sink(&self) -> Option<kithara_stream::BoxedEventSink>;
            /// Pull a clone of the optional byte-map handle from the
            /// inner source. Used by the decoder factory to activate the
            /// segment-by-segment fMP4 path on HLS.
            pub(crate) fn byte_map(&self) -> Option<Arc<dyn ByteMap>>;
            /// Narrow mutating playhead handle.
            pub(crate) fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            /// Narrow seek-control handle.
            pub(crate) fn seek_control(&self) -> Arc<dyn SeekControl>;
            /// Narrow seek-observe handle.
            pub(crate) fn seek_observe(&self) -> Arc<dyn SeekObserve>;
            /// Narrow activity handle.
            pub(crate) fn activity(&self) -> Arc<dyn Activity>;
            /// Overall source readiness at current position.
            pub(crate) fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// The reader→peer wake handle — `Some` for segmented sources
            /// (HLS) that push a downloader peer. The FSM arms it on the
            /// produce core (seek-apply / finalize); the scheduler shell
            /// flushes it off the forbid-blocking path.
            pub(crate) fn peer_wake(&self) -> Option<Arc<kithara_stream::DeferredWake>>;
            /// Install the audio worker's data-arrival wake on the inner
            /// source. Segmented sources (HLS) fire it from their off-RT
            /// write/settle sites; no-op for non-segmented sources. Set once,
            /// after the worker exists.
            pub(crate) fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>);
            /// Real-time on-core seek (FSM recreate/boundary, decoder
            /// `OffsetReader`): position math + cursor set, no `prime_seek_range`
            /// spin on the forbid-blocking produce core. See [`Stream::probe_seek`].
            pub(crate) fn probe_seek(&self, pos: SeekFrom) -> io::Result<u64>;
        }
    }
}

impl<T: StreamType> Clone for SharedStream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            blocking: Arc::clone(&self.blocking),
        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    /// Steady state (RT worker / `OffsetReader`): the non-blocking
    /// [`Stream::probe_read`] — a not-ready range surfaces immediately so the
    /// scheduler parks and re-ticks. During construction only (the `blocking`
    /// flag armed by `Audio::new`), routes through the blocking off-RT
    /// [`Stream::read`] adapter so the single up-front decoder build waits for
    /// residual init lateness instead of erroring on the first not-ready probe.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut stream = self.inner.lock();
        if self.blocking.load(Ordering::Acquire) {
            stream.read(buf)
        } else {
            stream.probe_read(buf)
        }
    }
}

impl<T: StreamType> Seek for SharedStream<T> {
    delegate! {
        to self.inner.lock() {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
        }
    }
}
