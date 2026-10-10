#![forbid(unsafe_code)]

use std::ops::Range;

use kithara_platform::{
    maybe_send::{MaybeSend, MaybeSync},
    sync::Arc,
    time::Duration,
};
use kithara_storage::WaitOutcome;
use kithara_test_utils::kithara;

use super::{ByteMap, ReadOutcome, SourcePhase, SourceProbe, VariantControl};
use crate::{
    activity::{Activity, ActivityWriter},
    error::StreamResult,
    media::MediaInfo,
    playhead::{PlayheadRead, PlayheadWrite},
    wake::{DeferredWake, WorkerWake},
};

/// Per-segment metadata exposed by segmented sources (HLS).
#[derive(Clone, Debug, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct SegmentDescriptor {
    /// Absolute decode time at the start of this segment (cumulative
    /// EXTINF over preceding segments).
    pub decode_time: Duration,
    /// Segment duration (EXTINF).
    pub duration: Duration,
    /// Byte range in the source's virtual stream.
    pub byte_range: Range<u64>,
    /// Segment index within the variant.
    pub segment_index: u32,
    /// Variant the descriptor was resolved against.
    pub variant_index: usize,
}

/// Time-first seek anchor resolved by a segmented source.
///
/// Represents a deterministic mapping from target playback time to a byte
/// position and segment context inside the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bon::Builder)]
#[non_exhaustive]
pub struct SourceSeekAnchor {
    #[builder(default)]
    pub segment_start: Duration,
    pub segment_end: Option<Duration>,
    pub segment_index: Option<u32>,
    pub variant_index: Option<usize>,
    #[builder(default)]
    pub byte_offset: u64,
}

/// Sync random-access source.
///
/// Provides sync interface for waiting and reading data at arbitrary offsets.
/// Reader wraps this directly to provide `Read + Seek`.
///
/// Methods take `&mut self` to allow sources to maintain internal state
/// (e.g., progress tracking, segment index updates).
#[kithara::mock(api = SourceMock)]
pub trait Source: MaybeSend + MaybeSync + 'static {
    /// Current ABR handle for runtime mode/bandwidth control.
    ///
    /// Adaptive sources (HLS) return the peer's `AbrHandle` so callers —
    /// queue, FFI, UI — can switch variant or cap bandwidth mid-playback.
    /// Non-adaptive sources (File) keep the default `None`.
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        None
    }

    /// Narrow handle to the playback-activity flag.
    fn activity(&self) -> Activity;

    /// Transfer the single publisher to the owning audio chain.
    fn take_activity_writer(&mut self) -> Option<ActivityWriter>;

    /// Advance the byte cursor by `n` bytes after a successful read.
    fn advance(&self, n: u64);

    /// Optional shared segment-layout handle for segment-aware decoders.
    ///
    /// Segment-aware decoders (fMP4 segment demuxer) call this once at
    /// open to grab a lock-free, Arc-shareable view over the segment
    /// table — independent of the byte cursor passed to the decoder
    /// through `Read + Seek`. Default `None` for non-segmented sources.
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        None
    }

    /// Whether the source currently reports zero bytes. Default mirrors
    /// `self.len()` returning `0` (or being unknown — both are treated as
    /// "no readable bytes yet" for the conventional `len`/`is_empty` pair).
    fn is_empty(&self) -> bool {
        self.len().is_none_or(|n| n == 0)
    }

    /// Total length if known.
    ///
    /// Streaming sources may block briefly until the HTTP response headers
    /// arrive (Content-Length discovery).
    fn len(&self) -> Option<u64>;

    /// Get media info if available.
    fn media_info(&self) -> Option<MediaInfo> {
        None
    }

    /// The reader→peer wake handle, if this source pushes a downloader peer.
    ///
    /// Segmented sources (HLS) return their [`DeferredWake`]; the driver
    /// reading or seeking the stream arms it on the produce core (the audio
    /// shell flushes it) or [`notify_now`](DeferredWake::notify_now)s it
    /// off-core, per the driver's own statically-known context. Non-segmented
    /// sources have no peer and return `None`.
    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        None
    }

    /// Overall source readiness at the current timeline position.
    ///
    /// Uses the source's internal knowledge of chunk/segment boundaries
    /// to determine if the next read operation can proceed without blocking.
    ///
    /// Unlike `phase_at(range)` which checks a specific byte range,
    /// this method lets the source decide the appropriate granularity.
    ///
    /// Default checks a single byte at the current position.
    /// HLS overrides with segment-aware logic, File with 32KB-window logic.
    fn phase(&self) -> SourcePhase {
        let pos = self.position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    /// Point-in-time snapshot of the source phase for the given range.
    ///
    /// Returns the current [`SourcePhase`] without blocking. Used internally
    /// by `wait_range()` implementations for fast-path dispatch.
    fn phase_at(&self, range: Range<u64>) -> SourcePhase;

    /// Narrow read-only handle to the playhead position and total duration.
    fn playhead_read(&self) -> Arc<dyn PlayheadRead>;

    /// Narrow mutating handle to the playhead — for the decode/produce path.
    fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;

    /// Current byte position in the source's virtual byte space.
    ///
    /// HLS delegates to active variant; file owns its own atomic cursor.
    fn position(&self) -> u64;

    /// Narrow byte-space handle serving the same snapshots as
    /// [`Source::phase_at`], [`Source::position`], [`Source::len`], and
    /// [`Source::byte_map`] without going through the stream's control-plane
    /// lock. The forbid-blocking audio produce core answers every byte-space
    /// question through this handle only.
    fn probe(&self) -> Arc<dyn SourceProbe>;

    /// Read data at offset into buffer.
    ///
    /// Returns [`ReadOutcome::Bytes`] with a non-zero byte count on
    /// progress, [`ReadOutcome::Pending`] with a typed
    /// [`super::PendingReason`] when no progress is possible this call (seek
    /// pending, variant fence, eviction), or [`ReadOutcome::Eof`] at
    /// natural end-of-stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails or the source is in an invalid state.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;

    /// Absolute set of the byte cursor — used by [`crate::Stream::seek`] and
    /// post-seek landings. Sources implement this via the same atomic
    /// cursor that backs [`Self::position`] / [`Self::advance`].
    fn set_position(&self, pos: u64);

    /// Install the audio worker's data-arrival wake (the inverse of
    /// [`peer_wake`](Self::peer_wake)). Segmented sources (HLS) store it and
    /// fire it from their off-RT write/commit sites so an underran worker
    /// re-ticks the instant bytes land, instead of polling. Set once, after
    /// the worker exists; the default is a no-op for sources whose data is
    /// always resident (local files, mocks), where the worker never underruns
    /// on a download.
    fn set_worker_wake(&self, _wake: Arc<dyn WorkerWake>) {}

    /// Build a fresh reader-side event-sink instance.
    ///
    /// Returned by Source-impls that want to expose reader-side events
    /// (`HlsSource`, `FileSource`). The audio pipeline takes the sink
    /// at decoder creation/recreation time and threads it into the
    /// composed decoder. Default `None` keeps mock and test sources
    /// without a sink.
    ///
    /// Each call must return a **fresh** sink instance, because decoder
    /// recreation (ABR / format change) rebuilds the decoder and the new
    /// sink needs a clean state cursor.
    fn take_reader_event_sink(&mut self) -> Option<crate::BoxedEventSink> {
        None
    }

    /// Optional HLS-only variant-coordination handle. Adaptive sources (HLS)
    /// return `Some`; non-adaptive sources keep the default `None`.
    fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        None
    }

    /// Wait for `range`; `None` waits for readiness or source cancellation without a wall-clock budget.
    /// A supplied timeout permits an implementation-defined non-ready outcome and cooperative worker yield.
    ///
    /// # Errors
    /// Returns cancellation or underlying storage failures.
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use kithara_test_utils::kithara;

    use super::*;
    use crate::{ActivityWriter, PlayheadState};

    /// Constant-phase probe for the minimal test sources below; shares the
    /// source's cursor cell and mirrors its `len`.
    struct FixedPhase {
        position: Arc<AtomicU64>,
        len: Option<u64>,
        phase: SourcePhase,
    }

    impl SourceProbe for FixedPhase {
        fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
            None
        }
        fn len(&self) -> Option<u64> {
            self.len
        }
        fn phase(&self) -> SourcePhase {
            self.phase
        }
        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            self.phase
        }
        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }
        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }
    }

    #[kithara::test]
    fn test_source_trait_object_safety() {
        fn _accepts_source<S: Source>(_s: S) {}
    }

    #[kithara::test]
    fn source_phase_defaults_to_waiting() {
        assert_eq!(SourcePhase::default(), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn phase_default_delegates_to_phase_at() {
        struct ReadySource {
            activity: Activity,
            activity_writer: Option<ActivityWriter>,
            playhead: Arc<PlayheadState>,
            position: Arc<AtomicU64>,
        }
        impl Source for ReadySource {
            fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
            }
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
            }
            fn activity(&self) -> Activity {
                self.activity.clone()
            }
            fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
                self.activity_writer.take()
            }
            fn wait_range(
                &mut self,
                _range: Range<u64>,
                _timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome> {
                Ok(WaitOutcome::Ready)
            }
            fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
                Ok(ReadOutcome::Eof)
            }
            fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
                SourcePhase::Ready
            }
            fn probe(&self) -> Arc<dyn SourceProbe> {
                Arc::new(FixedPhase {
                    phase: SourcePhase::Ready,
                    len: Some(100),
                    position: Arc::clone(&self.position),
                })
            }
            fn len(&self) -> Option<u64> {
                Some(100)
            }
            fn position(&self) -> u64 {
                self.position.load(Ordering::Acquire)
            }
            fn advance(&self, n: u64) {
                self.position.fetch_add(n, Ordering::AcqRel);
            }
            fn set_position(&self, pos: u64) {
                self.position.store(pos, Ordering::Release);
            }
        }
        let writer = ActivityWriter::new();
        let source = ReadySource {
            activity: writer.reader(),
            activity_writer: Some(writer),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
        };
        assert_eq!(source.phase(), SourcePhase::Ready);
    }

    /// Read-only source accessors follow their sole playhead and activity writers.
    #[kithara::test]
    fn narrow_source_accessors_seam() {
        struct MinimalSource {
            activity: Activity,
            activity_writer: Option<ActivityWriter>,
            playhead: Arc<PlayheadState>,
            position: Arc<AtomicU64>,
        }
        impl Source for MinimalSource {
            fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
            }
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
                Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
            }
            fn activity(&self) -> Activity {
                self.activity.clone()
            }
            fn take_activity_writer(&mut self) -> Option<ActivityWriter> {
                self.activity_writer.take()
            }
            fn wait_range(
                &mut self,
                _range: Range<u64>,
                _timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome> {
                Ok(WaitOutcome::Ready)
            }
            fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
                Ok(ReadOutcome::Eof)
            }
            fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
                SourcePhase::Waiting
            }
            fn probe(&self) -> Arc<dyn SourceProbe> {
                Arc::new(FixedPhase {
                    phase: SourcePhase::Waiting,
                    len: None,
                    position: Arc::clone(&self.position),
                })
            }
            fn len(&self) -> Option<u64> {
                None
            }
            fn position(&self) -> u64 {
                self.position.load(Ordering::Acquire)
            }
            fn advance(&self, n: u64) {
                self.position.fetch_add(n, Ordering::AcqRel);
            }
            fn set_position(&self, pos: u64) {
                self.position.store(pos, Ordering::Release);
            }
        }

        let writer = ActivityWriter::new();
        let mut src = MinimalSource {
            activity: writer.reader(),
            activity_writer: Some(writer),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
        };

        assert_eq!(src.playhead_read().position(), Duration::ZERO);
        assert_eq!(src.playhead_read().duration(), None);

        let mut writer = src.take_activity_writer().expect("sole activity writer");
        assert!(src.take_activity_writer().is_none());
        let snapshot = writer.reader();
        let clone = snapshot.clone();
        assert!(!snapshot.is_playing());
        assert!(!clone.is_playing());
        assert!(!writer.reader().is_playing());

        writer.set_playing(true);
        assert!(snapshot.is_playing());
        assert!(clone.is_playing());
        assert!(writer.reader().is_playing());
        assert!(snapshot.clone().is_playing());
        assert!(clone.clone().is_playing());

        assert!(snapshot.is_playing());
        writer.set_playing(false);
        assert!(!snapshot.is_playing());
        writer.set_playing(true);
        assert!(snapshot.is_playing());
    }
}
