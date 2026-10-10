use std::{
    io::{self, Read, Seek, SeekFrom},
    ops::Range,
};

use delegate::delegate;
use kithara_abr::AbrHandle;
use kithara_platform::sync::{Arc, Mutex};
use kithara_stream::{
    ByteMap, ConstructionGate, DeferredWake, MediaInfo, OpenedReader, PlayheadWrite, SourceProbe,
    SourceSeekAnchor, Stream, StreamResult, StreamType, VariantControl,
    format_change_segment_range, resolve_seek_target,
};

use super::offset::OffsetReader;

/// Shared stream wrapper for format change detection.
///
/// Wraps Stream in `Arc<Mutex>` to allow:
/// - Decoder to read via Read + Seek
/// - `StreamAudioSource` to check `media_info()` for format changes
#[derive_where::derive_where(Clone; T: StreamType)]
pub(crate) struct SharedStream<T: StreamType> {
    inner: Arc<Mutex<Stream<T>>>,
    /// Narrow byte-space handle. RT polls — phase, cursor, length, byte
    /// map — answer from here and never take `inner`: off-RT holders (a
    /// construction reader parked in `Stream::read`, a consumer query)
    /// hold that mutex across waits, and any contended acquire blocks the
    /// forbid-blocking produce core.
    probe: Arc<dyn SourceProbe>,
    /// Fixed-at-open handles the produce core reaches without `inner`;
    /// resolved once in [`Self::new`] before the stream enters the mutex.
    abr: Option<AbrHandle>,
    /// Construction mode for one decoder reader. Coordinator clones carry no
    /// gate; every opened reader receives a fresh gate so an off-RT rebuild
    /// cannot switch the active decoder to blocking I/O.
    construction_gate: Option<ConstructionGate>,
    peer_wake: Option<Arc<DeferredWake>>,
    variants: Option<Arc<dyn VariantControl>>,
}

impl<T: StreamType> SharedStream<T> {
    pub(crate) fn new(stream: Stream<T>) -> Self {
        let probe = stream.probe();
        let abr = stream.abr_handle();
        let variants = stream.variant_control();
        let peer_wake = stream.peer_wake();
        Self {
            probe,
            abr,
            variants,
            peer_wake,
            inner: Arc::new(Mutex::new(stream)),
            construction_gate: None,
        }
    }

    pub(crate) fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
        self.variants.clone()
    }

    /// Header byte range for decoder recreate after a format change — via
    /// the fixed variant-control handle; same answer as
    /// [`Stream::format_change_segment_range`].
    pub(crate) fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
        format_change_segment_range(self.variants.as_deref())
    }

    pub(crate) fn open_initial_reader(&self) -> OpenedReader {
        let construction_gate = ConstructionGate::default();
        OpenedReader::new(
            self.with_construction_gate(construction_gate.clone()),
            self.len(),
            self.byte_map(),
            Some(construction_gate),
            self.take_reader_event_sink(),
        )
    }

    pub(crate) fn open_rebuild_reader(&self, base_offset: u64) -> OpenedReader {
        let byte_len = self.len().map(|length| length.saturating_sub(base_offset));
        let byte_map = self.byte_map();
        let event_sink = self.take_reader_event_sink();
        let construction_gate = ConstructionGate::default();
        let input = OffsetReader::new(
            self.with_construction_gate(construction_gate.clone()),
            base_offset,
        );
        OpenedReader::new(
            input,
            byte_len,
            byte_map,
            Some(construction_gate),
            event_sink,
        )
    }

    /// Real-time on-core seek (FSM recreate/boundary, decoder
    /// `OffsetReader`): cursor math + cursor set through the probe, no
    /// lock and no `prime_seek_range` spin on the forbid-blocking produce
    /// core — the same [`resolve_seek_target`] math as the off-RT
    /// [`Seek::seek`]. The load→resolve→store is not atomic: it relies on
    /// the single-cursor-writer invariant — the produce core owns the
    /// cursor except while it is parked in `RebuildingDecoder`, the only
    /// window where the off-RT rebuild reader moves it instead.
    ///
    /// # Errors
    ///
    /// See [`resolve_seek_target`].
    pub(crate) fn probe_seek(&self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = resolve_seek_target(pos, self.probe.position(), self.probe.len())?;
        self.probe.set_position(new_pos);
        if let Some(ref wake) = self.peer_wake {
            wake.arm();
        }
        Ok(new_pos)
    }

    fn with_construction_gate(&self, construction_gate: ConstructionGate) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            probe: Arc::clone(&self.probe),
            abr: self.abr.clone(),
            variants: self.variants.clone(),
            peer_wake: self.peer_wake.clone(),
            construction_gate: Some(construction_gate),
        }
    }

    delegate! {
        to self.probe {
            pub(crate) fn phase(&self) -> kithara_stream::SourcePhase;
            /// Current read position — the source's atomic cursor.
            pub(crate) fn position(&self) -> u64;
            /// Absolute byte cursor set — forwards to the inner source's
            /// atomic, used post-seek when the audio FSM lands at a known
            /// byte position.
            pub(crate) fn set_position(&self, pos: u64);
            /// Total length if known.
            pub(crate) fn len(&self) -> Option<u64>;
            /// Optional byte-map handle; the decoder factory uses it to
            /// activate the segment-by-segment fMP4 path.
            pub(crate) fn byte_map(&self) -> Option<Arc<dyn ByteMap>>;
        }
        to self.abr {
            /// Runtime ABR handle — `Some` for adaptive sources (HLS).
            #[call(clone)]
            pub(crate) fn abr_handle(&self) -> Option<AbrHandle>;
        }
        to self.peer_wake {
            /// The reader→peer wake handle — `Some` for segmented sources
            /// (HLS) that push a downloader peer.
            #[call(clone)]
            pub(crate) fn peer_wake(&self) -> Option<Arc<DeferredWake>>;
        }
    }

    delegate! {
        to self.inner.lock() {
            pub(crate) fn media_info(&self) -> Option<MediaInfo>;
            pub(crate) fn seek_time_anchor(&self, position: kithara_platform::time::Duration) -> Result<Option<SourceSeekAnchor>, io::Error>;
            /// Build a fresh reader-side event-sink instance from the inner source.
            pub(crate) fn take_reader_event_sink(&self) -> Option<kithara_stream::BoxedEventSink>;
            /// Narrow mutating playhead handle.
            pub(crate) fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;

        }
    }
}

impl<T: StreamType> Read for SharedStream<T> {
    /// Steady state (RT worker / `OffsetReader`): the non-blocking
    /// [`Stream::probe_read`] — a not-ready range surfaces immediately so the
    /// scheduler parks and re-ticks. During this reader's construction only,
    /// routes through the blocking off-RT [`Stream::read`] adapter so
    /// construction waits for residual init lateness instead of erroring on
    /// the first not-ready probe.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut stream = self.inner.lock();
        if self
            .construction_gate
            .as_ref()
            .is_some_and(ConstructionGate::is_armed)
        {
            stream.read(buf)
        } else {
            stream.probe_read(buf)
        }
    }
}

impl<T: StreamType> Seek for SharedStream<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self
            .construction_gate
            .as_ref()
            .is_some_and(ConstructionGate::is_armed)
        {
            self.inner.lock().seek(pos)
        } else {
            self.probe_seek(pos)
        }
    }
}
