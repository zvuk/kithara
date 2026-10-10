#![forbid(unsafe_code)]

use std::{io, ops::Range};

use kithara_platform::{sync::Arc, time::Duration};

use super::{super::format_change_segment_range, contract::StreamType};
use crate::{
    DeferredWake, MediaInfo, SourcePhase, SourceSeekAnchor,
    activity::{Activity, ActivityWriter},
    error::{SourceError, StreamResult},
    playhead::PlayheadWrite,
    source::{Source, VariantControl},
};

/// Generic audio stream with sync `Read + Seek`.
///
/// `T` is a marker type defining the stream source (`Hls`, `File`, etc.).
/// Stream holds the source directly and implements `Read + Seek` by calling
/// `Source::wait_range()` and `Source::read_at()`.
pub struct Stream<T: StreamType> {
    pub(in crate::stream) source: T::Source,
}

impl<T: StreamType> Stream<T> {
    /// Create a new stream from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying stream source cannot be created.
    pub async fn new(config: T::Config) -> Result<Self, SourceError> {
        let source = T::create(config).await?;
        Ok(Self { source })
    }

    /// Header byte range for decoder recreate after a format change.
    ///
    /// # Errors
    ///
    /// See [`format_change_segment_range`].
    pub fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        format_change_segment_range(self.source.variant_control().as_deref())
    }

    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|len| len == 0)
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
            .map_err(|e| io::Error::other(e.to_string()))
    }

    delegate::delegate! {
        to self.source {
            /// Overall source readiness at current position.
            pub fn phase(&self) -> SourcePhase;
            /// Point-in-time readiness for a specific byte range.
            pub fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            /// Narrow byte-space handle — the same snapshots as
            /// [`Stream::phase_at`] / [`Stream::position`] / [`Stream::len`] /
            /// [`Stream::byte_map`] for callers that must not take the lock a
            /// shared stream wrapper puts around `Stream`.
            #[must_use]
            pub fn probe(&self) -> Arc<dyn crate::SourceProbe>;
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
            /// Read-only playback-activity snapshot.
            #[must_use]
            pub fn activity(&self) -> Activity;
            /// Transfer the sole publisher to the audio chain.
            pub fn take_activity_writer(&mut self) -> Option<ActivityWriter>;
            /// Narrow mutating playhead handle — position + duration.
            #[must_use]
            pub fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            /// Get current read position.
            pub fn position(&self) -> u64;
            /// Optional HLS-only variant-coordination handle — `Some` for adaptive
            /// sources (HLS), `None` otherwise.
            #[must_use]
            pub fn variant_control(&self) -> Option<Arc<dyn VariantControl>>;
        }
    }
}
