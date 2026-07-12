#![forbid(unsafe_code)]

use std::{
    error::Error as StdError,
    fmt,
    future::Future,
    io::{self, Error as IoError, ErrorKind, Read, Seek, SeekFrom},
    num::NonZeroUsize,
    ops::Range,
};

use kithara_platform::{
    maybe_send::{MaybeSend, MaybeSync},
    sync::Arc,
    time::Duration,
    tokio::task,
};
use kithara_storage::WaitOutcome;
use kithara_test_utils::kithara;

use crate::{
    DeferredWake, MediaInfo, SourcePhase, SourceSeekAnchor,
    error::{SourceError, StreamError, StreamResult},
    playhead::PlayheadWrite,
    seek_state::{Activity, SeekControl, SeekObserve},
    source::{NotReadyCause, PendingReason, ReadOutcome, Source, VariantControl},
};

/// Real error from [`Stream::try_read`] — the underlying source
/// surfaced an I/O failure.
///
/// Status conditions (seek pending, data not ready, variant change,
/// retry) are **not** errors and are carried in
/// [`StreamReadOutcome::Pending`] with a typed [`PendingReason`]. Only
/// genuine source failures end up here.
#[derive(Debug)]
#[non_exhaustive]
pub enum StreamReadError {
    /// Anything surfaced by the underlying [`Source`] as a real error.
    Source(IoError),
}

/// Outcome of a [`Stream::try_read`] call.
///
/// Mirrors the [`ReadOutcome`] shape from
/// [`Source::read_at`](crate::Source::read_at), but extends each variant
/// with the authoritative `byte_position` from the source cursor for
/// callers that don't want to read it back themselves.
/// `Bytes` carries a [`NonZeroUsize`] count — the type system
/// guarantees forward progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReadOutcome {
    /// Stream produced `count` bytes. `byte_position` is the new byte
    /// offset **after** the read.
    Bytes {
        count: NonZeroUsize,
        byte_position: u64,
    },
    /// No progress this call. See [`PendingReason`] for the precise
    /// cause and required caller action.
    Pending(PendingReason),
    /// Natural end of stream. `byte_position` is the offset where EOF
    /// was observed (typically the source length).
    Eof { byte_position: u64 },
}

/// Typed error from [`Stream::seek`] for an absolute byte target that
/// lands beyond the stream's known length.
///
/// Surfaced as the typed payload of an `io::Error` (kind
/// [`ErrorKind::InvalidInput`]) so consumers like Symphonia preserve it
/// through their own error chain. Decoders downcast to recover the
/// structured info and classify the failure as caller-side (the seek
/// target is invalid for this stream, not a decoder state corruption).
#[derive(Debug, Clone, Copy)]
pub struct StreamSeekPastEof {
    pub current_pos: u64,
    pub len: u64,
    pub new_pos: u64,
}

impl fmt::Display for StreamSeekPastEof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seek past EOF: new_pos={} len={} current_pos={}",
            self.new_pos, self.len, self.current_pos
        )
    }
}

impl StdError for StreamSeekPastEof {}

impl fmt::Display for StreamReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(e) => write!(f, "source error: {e}"),
        }
    }
}

impl StdError for StreamReadError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Source(e) => Some(e),
        }
    }
}

/// Typed payload of an `io::Error` (kind [`ErrorKind::Interrupted`])
/// emitted by `impl Read for Stream` when the underlying source could
/// not satisfy the read this call. Both `SeekPending` and
/// `NotReady`/`Retry` surface as `Interrupted` so demuxers (notably
/// Symphonia's fragmented MP4 reader) treat the pause as a transient
/// cooperative interruption and let `kithara-decode::is_seek_pending_io`
/// classify the failure correctly — the previous `WouldBlock` mapping
/// was treated as a hard "would block" by Symphonia's seek path and
/// corrupted the demuxer cursor on partial reads. Carries the
/// [`PendingReason`] verbatim plus a snapshot of source/timeline state
/// at the wrap site, so callers downcasting from `io::Error` recover
/// both *what* stalled and *why* without having to instrument their
/// own decoder.
#[derive(Debug, Clone, Copy)]
pub struct StreamPending {
    pub len: Option<u64>,
    pub reason: PendingReason,
    pub phase: SourcePhase,
    pub flushing: bool,
    pub variant_fence: bool,
    pub epoch: u64,
    pub pos: u64,
    pub want: usize,
}

impl fmt::Display for StreamPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: pos={} want={} len={:?} phase={:?} epoch={} flushing={} variant_fence={}",
            self.reason,
            self.pos,
            self.want,
            self.len,
            self.phase,
            self.epoch,
            self.flushing,
            self.variant_fence,
        )
    }
}

impl StdError for StreamPending {}

/// Non-retriable cross-variant boundary signal — the typed payload of
/// the `io::Error` produced by `impl Read for Stream` when the
/// underlying source fenced on a variant change. Decoders that go
/// through `std::io::Read` (Symphonia chain walker) downcast on this
/// type to recover the precise classification without string-matching.
#[derive(Debug, Clone, Copy)]
pub struct VariantChangeError;

impl fmt::Display for VariantChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("variant change: decoder recreation required")
    }
}

impl StdError for VariantChangeError {}

/// Defines a stream type and how to create it.
///
/// This trait is implemented by marker types (`Hls`, `File`) in their respective crates.
/// The implementation provides the config type and source type.
///
/// On wasm32, `Send`/`Sync` bounds are relaxed via [`MaybeSend`]/[`MaybeSync`].
pub trait StreamType: MaybeSend + 'static {
    /// Configuration for this stream type.
    type Config: Default + MaybeSend;

    /// Event bus type carried by the stream config.
    ///
    /// Concrete stream types set this to `kithara_events::EventBus`.
    /// `Audio::new()` constrains `T::Events = EventBus` to extract it.
    type Events: Clone + MaybeSend + MaybeSync + 'static;

    /// Source implementing `Source`.
    type Source: Source;

    /// Create the source from configuration.
    ///
    /// May also start background tasks (downloader) internally.
    fn create(config: Self::Config) -> impl Future<Output = Result<Self::Source, SourceError>>;

    /// Extract the event bus from config (if set).
    ///
    /// Used by `Audio::new()` to share a single bus across the stream
    /// and the audio pipeline.
    fn event_bus(config: &Self::Config) -> Option<Self::Events> {
        let _ = config;
        None
    }
}

/// Generic audio stream with sync `Read + Seek`.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds the source directly and implements `Read + Seek` by calling
/// `Source::wait_range()` and `Source::read_at()`.
pub struct Stream<T: StreamType> {
    source: T::Source,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying stream source cannot be created.
    pub async fn new(config: T::Config) -> Result<Self, SourceError> {
        let source = T::create(config).await?;
        task::yield_now().await;
        Ok(Self { source })
    }

    /// Narrow activity handle — set/query the `PLAYING` flag.
    #[must_use]
    pub fn activity(&self) -> Arc<dyn Activity> {
        self.source.activity()
    }

    /// Clear the variant fence, allowing reads from the next variant.
    /// No-op for non-adaptive sources.
    pub fn clear_variant_fence(&mut self) {
        if let Some(vc) = self.source.variant_control() {
            vc.clear_variant_fence();
        }
    }

    /// Header byte range for decoder recreate after a format change.
    ///
    /// # Errors
    ///
    /// `Err(SourceError::FormatChangeNotApplicable)` for non-HLS sources
    /// or HLS variants activated with `served_from > 0` (init prefix
    /// unreachable via Stream reads).
    pub fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        self.source.variant_control().map_or(
            Err(StreamError::Source(SourceError::FormatChangeNotApplicable)),
            |vc| vc.format_change_segment_range(),
        )
    }

    /// `true` while a cross-variant fence keeps `read_at` / `wait_range`
    /// short-circuited to `Pending(VariantChange)` / `Interrupted`.
    #[must_use]
    pub fn has_variant_change_pending(&self) -> bool {
        self.source
            .variant_control()
            .is_some_and(|vc| vc.has_variant_change_pending())
    }

    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
    }

    /// Narrow mutating playhead handle — position + duration.
    #[must_use]
    pub fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        self.source.playhead_write()
    }

    /// Get current read position.
    pub fn position(&self) -> u64 {
        self.source.position()
    }

    /// Narrow seek-control handle — begin / complete / `mark_pending`.
    #[must_use]
    pub fn seek_control(&self) -> Arc<dyn SeekControl> {
        self.source.seek_control()
    }

    /// Narrow seek-observe handle — read seek state without mutation.
    #[must_use]
    pub fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        self.source.seek_observe()
    }

    /// Resolve a deterministic time-based seek anchor.
    ///
    /// Returns `None` for sources without segmented time mapping.
    ///
    /// # Errors
    ///
    /// Returns an error when the source failed to resolve the anchor.
    pub fn seek_time_anchor(
        &mut self,
        position: Duration,
    ) -> Result<Option<SourceSeekAnchor>, io::Error> {
        self.source
            .byte_map()
            .map_or(Ok(None), |m| m.anchor_at_time(position))
            .map_err(|e| IoError::other(e.to_string()))
    }

    /// Get shared reference to inner source.
    pub fn source(&self) -> &T::Source {
        &self.source
    }

    /// Target variant index of the pending fence, `None` when no fence
    /// is up (or the source is non-adaptive).
    #[must_use]
    pub fn variant_change_target(&self) -> Option<usize> {
        self.source
            .variant_control()
            .and_then(|vc| vc.variant_change_target())
    }

    /// Optional HLS-only variant-coordination handle — `Some` for adaptive
    /// sources (HLS), `None` otherwise.
    #[must_use]
    pub fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        self.source.variant_control()
    }

    delegate::delegate! {
        to self.source {
            /// Overall source readiness at current position.
            pub fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// Get current media info if known.
            pub fn media_info(&self) -> Option<MediaInfo>;
            /// Runtime ABR handle — `Some` for adaptive sources (HLS).
            pub fn abr_handle(&self) -> Option<kithara_abr::AbrHandle>;
            /// Get total length if known.
            pub fn len(&self) -> Option<u64>;
            /// The reader→peer wake handle — `Some` for segmented sources (HLS)
            /// that push a downloader peer, `None` otherwise.
            pub fn peer_wake(&self) -> Option<Arc<DeferredWake>>;
            /// Install the audio worker's data-arrival wake. Segmented sources
            /// fire it from their off-RT write/commit sites; no-op otherwise.
            pub fn set_worker_wake(&self, wake: Arc<dyn crate::WorkerWake>);
            /// Build a fresh reader-side event-sink instance from the inner source.
            pub fn take_reader_event_sink(&mut self) -> Option<crate::BoxedEventSink>;
            /// Optional byte-map handle for segment-aware decoders.
            pub fn byte_map(&self) -> Option<Arc<dyn crate::ByteMap>>;
            /// Absolute byte-position set — used by [`Stream::seek`] callers
            /// and audio FSM landings. Forwards to the source's atomic cursor.
            pub fn set_position(&self, pos: u64);
        }
    }
}

/// Per-probe wait policy threaded into [`Stream::try_read_with`]. Internal
/// plumbing, NOT a public knob — it selects the `Source::wait_range` timeout
/// from the caller's statically-known context.
#[derive(Clone, Copy)]
enum WaitMode {
    /// RT / cooperative-yield probe (`probe_read`): bounded
    /// `Some(WAIT_RANGE_TIMEOUT)`, returns without blocking.
    Probe,
    /// Off-RT consumer (`impl Read`): `None` — the source parks event-driven
    /// until the range resolves (no wall-clock poll at this layer).
    Block,
}

impl WaitMode {
    fn timeout(self, probe: Duration) -> Option<Duration> {
        match self {
            Self::Probe => Some(probe),
            Self::Block => None,
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Per-probe hint passed to [`Source::wait_range`] on the RT worker read
    /// path (`probe_read` → [`WaitMode::Probe`]). Non-blocking-pull sources
    /// (HLS) ignore the value and answer with a single readiness probe; the
    /// backoff between probes lives in the audio scheduler's `Waiting` park
    /// (10ms). The off-RT [`Read`] path passes `None` ([`WaitMode::Block`])
    /// instead and parks event-driven until the range resolves.
    const WAIT_RANGE_TIMEOUT: Duration = Duration::from_millis(10);

    /// Prime metadata for a seek target by blocking on
    /// [`Source::wait_range`]`(range, None)` until the range resolves
    /// (`Ready`/`Eof`), a segment fails, or the source's cancel fires —
    /// event-driven, **no** wall-clock budget. Runs on the consumer/seek
    /// thread, never the RT worker. The one-time peer wake re-aims the
    /// prefetch window at the new cursor so the awaited range starts
    /// downloading; give-up authority lives lower in the stack (the
    /// downloader's per-fetch inactivity timeout / the cancel hierarchy),
    /// which is why a timer here would only fire on a legitimately in-flight
    /// fetch. The outcome is advisory; `seek` re-checks `source.len()`
    /// afterwards.
    #[kithara::flash(true)]
    fn prime_seek_range(&mut self, range: Range<u64>) {
        // Cache-hit fast path: a non-blocking phase probe. A re-seek into
        // already-resident bytes needs no fetch and no wait — skip the peer
        // wake so a burst of random seeks over a warm cache does not fire one
        // cross-thread `notify_one` (and downloader re-plan) per seek.
        if matches!(
            self.source.phase_at(range.clone()),
            SourcePhase::Ready | SourcePhase::Eof
        ) {
            return;
        }
        // Not resident: wake the peer once so it re-aims its prefetch at the
        // new cursor, then block on the source's event-driven wait.
        if let Some(wake) = self.source.peer_wake() {
            wake.notify_now();
        }
        let _ = self.source.wait_range(range, None);
    }

    /// Typed read — returns a [`StreamReadOutcome`] discriminating
    /// progress (`Bytes` with [`NonZeroUsize`]) from non-progress
    /// (`Pending` with a typed [`PendingReason`]) and natural EOF.
    /// `impl Read for Stream` wraps this outcome for `std::io::Read`
    /// consumers.
    ///
    /// # Errors
    ///
    /// Returns [`StreamReadError::Source`] only when the underlying source
    /// reports a genuine I/O failure. Backpressure, seek-pending, and a
    /// variant fence are non-errors — they surface as `Ok(Pending(..))`.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<StreamReadOutcome, StreamReadError> {
        self.try_read_with(buf, WaitMode::Probe)
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    #[kithara::hang_watchdog]
    fn try_read_with(
        &mut self,
        buf: &mut [u8],
        wait: WaitMode,
    ) -> Result<StreamReadOutcome, StreamReadError> {
        if buf.is_empty() {
            return Ok(StreamReadOutcome::Eof {
                byte_position: self.source.position(),
            });
        }

        let variant_control = self.source.variant_control();
        let seek_obs = self.source.seek_observe();
        loop {
            let read_epoch = seek_obs.epoch();
            let pos = self.source.position();
            let range = pos..pos.saturating_add(buf.len() as u64);

            if variant_control
                .as_ref()
                .is_some_and(|vc| vc.has_variant_change_pending())
            {
                return Ok(StreamReadOutcome::Pending(PendingReason::VariantChange));
            }

            let wait_result = self
                .source
                .wait_range(range, wait.timeout(Self::WAIT_RANGE_TIMEOUT));
            let wait_outcome = match wait_result {
                Ok(outcome) => outcome,
                Err(StreamError::Source(SourceError::WaitBudgetExceeded)) => {
                    if seek_obs.is_flushing() || seek_obs.epoch() != read_epoch {
                        return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                    }
                    return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::WaitBudgetExhausted,
                    )));
                }
                Err(e) => {
                    return Err(StreamReadError::Source(IoError::other(e.to_string())));
                }
            };
            match wait_outcome {
                WaitOutcome::Ready => {}
                WaitOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                WaitOutcome::Interrupted => {
                    if seek_obs.is_flushing() {
                        return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                    }
                    return Ok(StreamReadOutcome::Pending(PendingReason::NotReady(
                        NotReadyCause::WaitInterrupted,
                    )));
                }
            }

            if seek_obs.epoch() != read_epoch {
                return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
            }

            match self
                .source
                .read_at(pos, buf)
                .map_err(|e| StreamReadError::Source(IoError::other(e.to_string())))?
            {
                ReadOutcome::Bytes(count) => {
                    if seek_obs.epoch() != read_epoch {
                        return Ok(StreamReadOutcome::Pending(PendingReason::SeekPending));
                    }
                    hang_reset!();
                    self.source.advance(count.get() as u64);
                    let new_pos = self.source.position();
                    return Ok(StreamReadOutcome::Bytes {
                        count,
                        byte_position: new_pos,
                    });
                }
                ReadOutcome::Eof => {
                    return Ok(StreamReadOutcome::Eof { byte_position: pos });
                }
                ReadOutcome::Pending(PendingReason::Retry) => {
                    // Resource evicted between `wait_range` (Ready) and `read_at`:
                    // re-acquire on the next loop. This is active progress, not a
                    // wait, so re-loop tightly — the reader stays counted and keeps
                    // the virtual clock pinned until it re-acquires. Yielding here
                    // would cede the clock to a peer's watchdog deadline mid-progress
                    // (under flash the yield jumps the clock to that deadline, firing
                    // a false hang before the re-acquire completes). The watchdog
                    // tick still bounds a genuine eviction storm in real time.
                    hang_tick!();
                    continue;
                }
                ReadOutcome::Pending(reason) => {
                    return Ok(StreamReadOutcome::Pending(reason));
                }
            }
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Worker (produce-core) peer wake: a reader-blocked probe arms the wake
    /// lock-free. The audio scheduler shell flushes it off the forbid path, so
    /// the cross-thread `notify_one` (a `kevent`) never fires on the RT core.
    /// No-op for sources without a peer (file streams).
    fn arm_peer_wake(&self) {
        if let Some(wake) = self.source.peer_wake() {
            wake.arm();
        }
    }

    /// Consumer (off-core) peer wake: a not-ready probe wakes the peer
    /// immediately — the consumer never runs on the RT produce core, so the
    /// `notify_one` is allowed and the read is not stalled on the worker's next
    /// pass. No-op for sources without a peer (file streams).
    fn notify_peer_wake(&self) {
        if let Some(wake) = self.source.peer_wake() {
            wake.notify_now();
        }
    }

    /// Map a single [`Self::try_read`] probe to the `std::io::Read`
    /// contract **without blocking**: a not-ready range surfaces as an
    /// `Interrupted`/`Other` `io::Error` carrying the typed
    /// [`StreamPending`]/[`VariantChangeError`] payload immediately.
    ///
    /// This is the real-time worker read path — the audio worker maps
    /// the `Interrupted`/`WouldBlock` error to `DemuxOutcome::Pending`
    /// and parks in the scheduler. Direct `Read + Seek` consumers go
    /// through the blocking [`Read::read`] adapter instead.
    ///
    /// # Errors
    ///
    /// Returns the source's `io::Error` for genuine failures, an
    /// `Interrupted` error carrying [`StreamPending`] for transient
    /// backpressure / seek-pending, and `Other(VariantChangeError)` at a
    /// variant boundary.
    pub fn probe_read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.try_read(buf) {
            Ok(StreamReadOutcome::Bytes { count, .. }) => Ok(count.get()),
            Ok(StreamReadOutcome::Eof { .. }) => Ok(0),
            Ok(StreamReadOutcome::Pending(reason @ PendingReason::SeekPending)) => {
                self.arm_peer_wake();
                Err(IoError::new(ErrorKind::Interrupted, reason))
            }
            Ok(StreamReadOutcome::Pending(
                reason @ (PendingReason::NotReady(_) | PendingReason::Retry),
            )) => {
                self.arm_peer_wake();
                Err(IoError::new(
                    ErrorKind::Interrupted,
                    self.snapshot_pending(reason, buf.len()),
                ))
            }
            Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                Err(IoError::other(VariantChangeError))
            }
            Err(StreamReadError::Source(e)) => Err(e),
        }
    }

    /// Real-time on-core seek: resolve + cursor set, with NO `prime_seek_range`
    /// spin — no `yield_now`/`notify_one` on the forbid-blocking produce core.
    /// The audio worker (the FSM's recreate/boundary seeks and the decoder's
    /// `OffsetReader`) seeks through this; the off-RT [`Seek::seek`] adapter
    /// primes inline instead. The seeked range's readiness is discovered by the
    /// next `probe_read` (park-on-not-ready), and the armed peer wake — flushed
    /// by the shell — drives the fetch.
    ///
    /// # Errors
    ///
    /// See [`Self::resolve_seek_target`].
    pub fn probe_seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = self.resolve_seek_target(pos, self.source.len())?;
        self.source.set_position(new_pos);
        // The reader cursor moved on the produce core: arm the peer so it
        // re-targets fetches around the new position. The shell flushes it.
        self.arm_peer_wake();
        Ok(new_pos)
    }

    /// Resolve a [`SeekFrom`] to an absolute, clamped byte target — the shared
    /// math behind the off-RT [`Seek::seek`] and the on-core [`Self::probe_seek`].
    ///
    /// `len` is the known source length, resolved by the caller (`seek` primes
    /// to discover it; `probe_seek` uses the current value); `SeekFrom::End`
    /// errors when it is `None`. `Current` is relative to the live cursor.
    ///
    /// # Errors
    ///
    /// `Unsupported` for `SeekFrom::End` without a known length; `InvalidInput`
    /// for a negative resulting offset.
    fn resolve_seek_target(&self, pos: SeekFrom, len: Option<u64>) -> io::Result<u64> {
        let new_pos: i128 = match pos {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::Current(delta) => {
                i128::from(self.source.position()).saturating_add(i128::from(delta))
            }
            SeekFrom::End(delta) => {
                let Some(len) = len else {
                    return Err(IoError::new(
                        ErrorKind::Unsupported,
                        "seek from end requires known length",
                    ));
                };
                i128::from(len).saturating_add(i128::from(delta))
            }
        };
        if new_pos < 0 {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                "negative seek position",
            ));
        }
        Ok(u64::try_from(new_pos).unwrap_or(u64::MAX))
    }
}

impl<T: StreamType> Read for Stream<T> {
    /// Blocking `std::io::Read` for direct `Read + Seek` consumers.
    ///
    /// `try_read` is a single non-blocking probe; this adapter waits across
    /// repeated probes so a `read` issued right after stream open or a seek
    /// blocks until the awaited bytes download, instead of erroring on the
    /// first not-ready probe. The wait is **not** bounded by a wall-clock
    /// budget here: give-up authority lives lower in the stack, where it can
    /// tell a slow-but-live transfer from a genuine stall. A fixed budget at
    /// this layer would fire while a fetch is legitimately in flight and
    /// surface as `Interrupted` — which decoders misread as a seek.
    ///
    /// The loop terminates on a terminal outcome:
    /// - `Bytes`/`Eof` — data delivered or natural end of stream;
    /// - `SeekPending` — a seek flushed mid-read (`Interrupted`);
    /// - `VariantChange` — a variant boundary;
    /// - a source `Err` — the downloader's per-chunk inactivity timeout
    ///   failed the fetch, or the cancel hierarchy fired on teardown
    ///   (`HlsCoord::wait_range` / the storage wait surface both as a
    ///   terminal error).
    ///
    /// The blocking wait lives inside [`try_read_with`](Self::try_read_with)'s
    /// `wait_range(_, None)` (event-driven — the source parks until the range
    /// resolves, no wall-clock poll at this layer). The `NotReady`/`Retry`
    /// re-loop is therefore reached only on an eviction `Retry` or a spurious
    /// wake, and the next probe parks again at once — never a busy spin. A
    /// genuine wedge (no signal site ever fires) is caught by the source's hang
    /// watchdog rather than masked here. This is the off-RT consumer interface —
    /// the real-time worker uses [`Self::probe_read`] and never blocks here.
    #[kithara::flash(true)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.try_read_with(buf, WaitMode::Block) {
                Ok(StreamReadOutcome::Bytes { count, .. }) => return Ok(count.get()),
                Ok(StreamReadOutcome::Eof { .. }) => return Ok(0),
                Ok(StreamReadOutcome::Pending(
                    PendingReason::NotReady(_) | PendingReason::Retry,
                )) => {
                    // Wake the peer (an evicted `Retry` range must be re-fetched),
                    // then re-loop — the next `try_read_with` parks in the
                    // event-driven `wait_range(_, None)`.
                    self.notify_peer_wake();
                }
                Ok(StreamReadOutcome::Pending(reason @ PendingReason::SeekPending)) => {
                    self.notify_peer_wake();
                    return Err(IoError::new(ErrorKind::Interrupted, reason));
                }
                Ok(StreamReadOutcome::Pending(PendingReason::VariantChange)) => {
                    return Err(IoError::other(VariantChangeError));
                }
                Err(StreamReadError::Source(e)) => return Err(e),
            }
        }
    }
}

impl<T: StreamType> Stream<T> {
    /// Build a typed [`StreamPending`] payload for a
    /// `Pending(NotReady|Retry)` surfaced through `impl Read`. Pulls
    /// live source/timeline state at the moment of the wrap so the
    /// resulting `io::Error` carries the real reason ("data not ready
    /// (`wait_budget_exhausted`): pos=N len=M phase=… epoch=E flushing=…
    /// `variant_fence`=…") instead of a bare "data not ready". Decoders
    /// downcast on `StreamPending` to recover the typed [`PendingReason`].
    fn snapshot_pending(&self, reason: PendingReason, want: usize) -> StreamPending {
        let pos = self.source.position();
        let len = self.source.len();
        let phase = self.source.phase_at(pos..pos.saturating_add(want as u64));
        let seek_obs = self.source.seek_observe();
        StreamPending {
            reason,
            pos,
            want,
            len,
            phase,
            epoch: seek_obs.epoch(),
            flushing: seek_obs.is_flushing(),
            variant_fence: self
                .source
                .variant_control()
                .is_some_and(|vc| vc.has_variant_change_pending()),
        }
    }
}

impl<T: StreamType> Seek for Stream<T> {
    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let current = self.source.position();

        // Off-RT consumer path: discover the length for an End-relative seek by
        // priming (the produce-core `probe_seek` cannot, and errors instead).
        if matches!(pos, SeekFrom::End(_)) && self.source.len().is_none() {
            self.prime_seek_range(0..1);
        }
        let new_pos = self.resolve_seek_target(pos, self.source.len())?;

        let wait_range = match self.format_change_segment_range() {
            Ok(range) if range.start == new_pos => range,
            _ => new_pos..new_pos.saturating_add(1),
        };
        self.prime_seek_range(wait_range);

        if let Some(len) = self.source.len()
            && new_pos > len
        {
            return Err(IoError::new(
                ErrorKind::InvalidInput,
                StreamSeekPastEof {
                    new_pos,
                    len,
                    current_pos: current,
                },
            ));
        }

        self.source.set_position(new_pos);
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicU64, Ordering},
    };

    use kithara_platform::sync::Arc;
    use kithara_storage::WaitOutcome;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{PlayheadRead, PlayheadState, ReadOutcome, SeekState, Source, SourcePhase};

    /// Test helper — script entry that maps to either `Bytes(N)` (with
    /// the source slicing actual `data`) or a terminal `Eof`. Pending
    /// causes are exercised through the seek state (`begin`) and
    /// the wait-outcome script, not the read script.
    #[derive(Clone, Copy)]
    enum ScriptRead {
        Data(usize),
        Eof,
    }

    fn bytes(count: usize) -> ReadOutcome {
        let nz = NonZeroUsize::new(count)
            .expect("BUG: ScriptSource::bytes invariant — count must be > 0");
        ReadOutcome::Bytes(nz)
    }

    struct ScriptSource {
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
        anchor: Option<SourceSeekAnchor>,
        peer_wake: Option<Arc<DeferredWake>>,
        data: Vec<u8>,
        reads: VecDeque<ScriptRead>,
        waits: VecDeque<WaitOutcome>,
    }

    impl ScriptSource {
        fn new(
            seek: Arc<SeekState>,
            waits: impl IntoIterator<Item = WaitOutcome>,
            reads: impl IntoIterator<Item = ScriptRead>,
            data: Vec<u8>,
        ) -> Self {
            Self {
                seek,
                data,
                playhead: Arc::new(PlayheadState::new()),
                position: Arc::new(AtomicU64::new(0)),
                anchor: None,
                reads: reads.into_iter().collect(),
                waits: waits.into_iter().collect(),
                peer_wake: None,
            }
        }

        fn with_peer_wake(mut self, wake: Arc<DeferredWake>) -> Self {
            self.peer_wake = Some(wake);
            self
        }
    }

    impl Source for ScriptSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn byte_map(&self) -> Option<Arc<dyn crate::ByteMap>> {
            Some(Arc::new(ScriptByteMap {
                anchor: self.anchor,
                len: self.data.len() as u64,
            }))
        }

        fn len(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }

        fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
            self.peer_wake.clone()
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Waiting
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            let step = self.reads.pop_front().unwrap_or(ScriptRead::Eof);
            match step {
                ScriptRead::Eof => Ok(ReadOutcome::Eof),
                ScriptRead::Data(n) => {
                    let Ok(start) = usize::try_from(offset) else {
                        return Ok(ReadOutcome::Eof);
                    };
                    let end = (start + n).min(self.data.len());
                    let bytes_count = end.saturating_sub(start).min(buf.len());
                    if bytes_count == 0 {
                        return Ok(ReadOutcome::Eof);
                    }
                    buf[..bytes_count].copy_from_slice(&self.data[start..start + bytes_count]);
                    Ok(bytes(bytes_count))
                }
            }
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            Ok(self.waits.pop_front().unwrap_or(WaitOutcome::Ready))
        }
    }

    struct ScriptByteMap {
        anchor: Option<SourceSeekAnchor>,
        len: u64,
    }

    impl crate::ByteMap for ScriptByteMap {
        fn anchor_at_time(&self, _position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
            Ok(self.anchor)
        }

        fn init_segment_range(&self) -> Range<u64> {
            0..0
        }

        fn len(&self) -> Option<u64> {
            Some(self.len)
        }

        fn segment_after_byte(&self, _byte_offset: u64) -> Option<crate::SegmentDescriptor> {
            None
        }

        fn segment_at_time(&self, _t: Duration) -> Option<crate::SegmentDescriptor> {
            None
        }

        fn segment_count(&self) -> Option<u32> {
            None
        }
    }

    struct DummyType;

    impl StreamType for DummyType {
        type Config = ();
        type Events = ();
        type Source = ScriptSource;

        async fn create(_config: Self::Config) -> Result<Self::Source, SourceError> {
            Err(SourceError::other(IoError::other("not used in unit tests")))
        }
    }

    struct SeekDuringWaitType;

    impl StreamType for SeekDuringWaitType {
        type Config = ();
        type Events = ();
        type Source = SeekDuringWaitSource;

        async fn create(_config: Self::Config) -> Result<Self::Source, SourceError> {
            Err(SourceError::other(IoError::other("not used in unit tests")))
        }
    }

    struct SeekDuringWaitSource {
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
        read_calls: usize,
    }

    impl Source for SeekDuringWaitSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn len(&self) -> Option<u64> {
            Some(4)
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            self.read_calls += 1;
            Ok(bytes(4))
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            let _ = SeekControl::begin(&*self.seek, Duration::from_millis(10));
            Ok(WaitOutcome::Ready)
        }
    }

    #[kithara::test]
    fn probe_read_arms_peer_wake_on_core_without_notifying() {
        // Worker read path: a not-ready probe ARMS the deferred wake (lock-free)
        // instead of waking the downloader cross-thread — that `notify_one` is a
        // `kevent` the RT produce core must not make. The scheduler shell flushes
        let wake = Arc::new(DeferredWake::default());
        let source = ScriptSource::new(
            Arc::new(SeekState::new()),
            [WaitOutcome::Interrupted],
            [],
            vec![0u8; 8],
        )
        .with_peer_wake(Arc::clone(&wake));
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream.probe_read(&mut buf);
        assert!(
            outcome.is_err(),
            "not-ready worker probe surfaces as an Interrupted io::Error"
        );
        assert!(
            wake.flush(),
            "the worker probe armed the deferred wake; the shell flush delivers it"
        );
        assert!(!wake.flush(), "the arm coalesced into a single delivery");
    }

    #[kithara::test]
    fn read_notifies_peer_wake_immediately_off_core() {
        // Consumer read path: a not-ready probe wakes the peer IMMEDIATELY (the
        // consumer is off the RT core), so a blocking read is not stalled
        // waiting for the worker's next pass. An immediate notify leaves nothing
        // armed — the distinguishing signal from the worker's deferred arm.
        let wake = Arc::new(DeferredWake::default());
        let source = ScriptSource::new(
            Arc::new(SeekState::new()),
            [WaitOutcome::Interrupted, WaitOutcome::Ready],
            [ScriptRead::Data(4)],
            b"ABCD".to_vec(),
        )
        .with_peer_wake(Arc::clone(&wake));
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let n = stream
            .read(&mut buf)
            .expect("read completes once the source reports the range ready");
        assert_eq!(n, 4);
        assert_eq!(&buf, b"ABCD");
        assert!(
            !wake.flush(),
            "the consumer path notifies immediately — nothing is left armed"
        );
    }

    #[kithara::test]
    fn probe_seek_moves_cursor_and_arms_without_priming() {
        // On-core seek: set the cursor and arm the peer (the shell flushes),
        // with NO prime_seek_range spin — so it returns immediately even when
        // the target range is not ready (the wait script is never consulted).
        let wake = Arc::new(DeferredWake::default());
        let source = ScriptSource::new(
            Arc::new(SeekState::new()),
            [WaitOutcome::Interrupted, WaitOutcome::Interrupted],
            [],
            vec![0u8; 100],
        )
        .with_peer_wake(Arc::clone(&wake));
        let mut stream = Stream::<DummyType> { source };

        let started = Instant::now();
        let pos = stream
            .probe_seek(SeekFrom::Start(42))
            .expect("absolute on-core seek within range");
        let elapsed = started.elapsed();

        assert_eq!(pos, 42);
        assert_eq!(stream.source.position(), 42, "cursor moved to the target");
        assert!(
            elapsed < Duration::from_millis(2),
            "probe_seek must not prime/spin (the consumer Seek would); took {elapsed:?}"
        );
        assert!(
            wake.flush(),
            "probe_seek armed the peer wake on the produce core; the shell flushes it"
        );
    }

    #[kithara::test]
    fn try_read_yields_not_ready_on_interrupted_then_recovers_next_probe() {
        let source = ScriptSource::new(
            Arc::new(SeekState::new()),
            [WaitOutcome::Interrupted, WaitOutcome::Ready],
            [ScriptRead::Data(4)],
            b"ABCD".to_vec(),
        );
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let first = stream
            .try_read(&mut buf)
            .expect("BUG: not-ready is a status return; not a hard error in this test");
        assert!(matches!(
            first,
            StreamReadOutcome::Pending(PendingReason::NotReady(NotReadyCause::WaitInterrupted))
        ));

        let n = stream
            .read(&mut buf)
            .expect("BUG: read must succeed once the source reports the range ready");
        assert_eq!(n, 4);
        assert_eq!(&buf, b"ABCD");
    }

    #[kithara::test]
    fn try_read_returns_seek_pending_when_flushing() {
        let seek = Arc::new(SeekState::new());
        let _ = SeekControl::begin(&*seek, Duration::from_millis(10));
        let source = ScriptSource::new(Arc::clone(&seek), [WaitOutcome::Interrupted], [], vec![]);
        let mut stream = Stream::<DummyType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream
            .try_read(&mut buf)
            .expect("BUG: seek-pending is a status return; not a hard error in this test");
        assert!(matches!(
            outcome,
            StreamReadOutcome::Pending(PendingReason::SeekPending)
        ));
    }

    #[kithara::test]
    fn try_read_returns_seek_pending_when_epoch_changes_after_wait() {
        let source = SeekDuringWaitSource {
            seek: Arc::new(SeekState::new()),
            playhead: Arc::new(PlayheadState::new()),
            position: Arc::new(AtomicU64::new(0)),
            read_calls: 0,
        };
        let mut stream = Stream::<SeekDuringWaitType> { source };
        let mut buf = [0u8; 4];

        let outcome = stream
            .try_read(&mut buf)
            .expect("BUG: seek-pending is a status return; not a hard error in this test");

        assert!(matches!(
            outcome,
            StreamReadOutcome::Pending(PendingReason::SeekPending)
        ));
        assert_eq!(stream.source.read_calls, 0);
        assert_eq!(stream.position(), 0);
    }

    #[kithara::test]
    fn seek_updates_position() {
        let source = ScriptSource::new(Arc::new(SeekState::new()), [], [], b"ABCDE".to_vec());
        let mut stream = Stream::<DummyType> { source };

        let pos = stream
            .seek(SeekFrom::Start(3))
            .expect("BUG: seek to a position within the test stream must succeed");

        assert_eq!(pos, 3);
        assert_eq!(stream.position(), 3);
    }

    #[kithara::test]
    fn seek_time_anchor_does_not_move_position() {
        let mut source = ScriptSource::new(Arc::new(SeekState::new()), [], [], b"ABCDE".to_vec());
        source.set_position(11);
        source.anchor = Some(SourceSeekAnchor {
            byte_offset: 3,
            segment_start: Duration::from_secs(8),
            segment_end: Some(Duration::from_secs(12)),
            segment_index: Some(2),
            variant_index: Some(1),
        });
        let mut stream = Stream::<DummyType> { source };

        let anchor = stream
            .seek_time_anchor(Duration::from_millis(8_500))
            .expect("BUG: anchor resolution must succeed for the constructed test stream")
            .expect("BUG: stream must return the resolved anchor in this test");

        assert_eq!(anchor.byte_offset, 3);
        assert_eq!(
            stream.position(),
            11,
            "anchor resolution must not eagerly commit stream position"
        );
    }
}
