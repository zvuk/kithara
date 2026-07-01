#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::{DeferredBus, EventBus, HlsEvent};
use kithara_stream::{PrerollHint, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};

use crate::stream::HlsCoord;

/// Ring depth for the decode-core → shell event hand-off. The hooks
/// resolve at most a segment-boundary and an initial-seek event per pass,
/// so this never fills in practice; it only bounds the worst-case burst.
const READER_EVENT_CAPACITY: usize = 256;

/// Decoder→HLS reader hook bridge: turns the decoder's per-chunk and
/// per-seek signals into [`HlsEvent`]s on the track's [`EventBus`].
///
/// Mirrors `kithara-file`'s `FileReaderEventSink` but resolves the
/// landed byte to its `(variant, segment_index, byte_in_segment)` triple
/// via the variant-aware [`HlsCoord::find_at_offset`] — this is what
/// makes the integration tests' `HlsEvent::ReaderSeek { segment_index,
/// .. }` assertion observable.
pub(crate) struct HlsReaderEventSink {
    coord: Arc<HlsCoord>,
    seek_epoch_handle: Arc<AtomicU64>,
    bus: DeferredBus<HlsEvent>,
    /// `(variant_index, segment_index)` of the last segment the
    /// reader was observed in. A change between `on_chunk` calls
    /// drives [`HlsEvent::SegmentReadStart`]; the same pair held
    /// across a seek is intentionally treated as a no-op for the
    /// boundary event (the seek itself was already announced).
    last_segment: Option<(usize, usize)>,
    initial_seek_published: bool,
    /// Cursor at hooks-creation time. After a `Seek` failure the
    /// decoder may discard the seek outcome and resume at the
    /// pre-seek cursor; we publish that fallback as a `ReaderSeek`
    /// the first time `on_chunk` fires so the bus subscriber sees a
    /// non-default `seek_epoch` even if `on_seek` never landed.
    initial_cursor: u64,
    last_cursor: u64,
}

impl HlsReaderEventSink {
    pub(crate) fn new(
        bus: EventBus,
        coord: Arc<HlsCoord>,
        seek_epoch_handle: Arc<AtomicU64>,
    ) -> Self {
        let last_cursor = coord.position();
        Self {
            bus: DeferredBus::new(bus, READER_EVENT_CAPACITY),
            coord,
            last_cursor,
            seek_epoch_handle,
            initial_cursor: last_cursor,
            initial_seek_published: false,
            last_segment: None,
        }
    }

    /// Announce a [`HlsEvent::SegmentReadStart`] when the reader's
    /// `cursor` lands in a different `(variant, segment_index)` than
    /// the previously observed one.
    fn maybe_publish_segment_start(&mut self, cursor: u64) {
        let Some((seg_idx, seg_start, _size)) = self.coord.find_at_offset(cursor) else {
            return;
        };
        let variant = self.coord.variant_index();
        let seg_us = seg_idx as usize;
        let key = (variant, seg_us);
        if self.last_segment == Some(key) {
            return;
        }
        self.last_segment = Some(key);
        self.bus.enqueue(HlsEvent::SegmentReadStart {
            variant,
            segment_index: seg_us,
            byte_offset: seg_start,
        });
    }

    fn publish_initial_seek(&mut self, cursor: u64) {
        if self.initial_seek_published {
            return;
        }
        self.initial_seek_published = true;
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        if seek_epoch == 0 {
            return;
        }
        self.publish_seek(self.initial_cursor, cursor);
    }

    fn publish_seek(&self, from: u64, to: u64) {
        let (variant, segment_index, byte_in_segment) = match self.coord.find_at_offset(to) {
            Some((seg, seg_start, _size)) => (
                Some(self.coord.variant_index()),
                Some(seg as usize),
                Some(to.saturating_sub(seg_start)),
            ),
            None => (None, None, None),
        };
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        self.bus.enqueue(HlsEvent::ReaderSeek {
            seek_epoch,
            variant,
            segment_index,
            byte_in_segment,
            from_offset: from,
            to_offset: to,
        });
    }
}

impl ReaderEventSink for HlsReaderEventSink {
    fn flush(&mut self) {
        self.bus.flush();
    }

    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        if !matches!(signal, ReaderChunkSignal::Chunk) {
            return;
        }
        let cursor = self.coord.position();
        self.publish_initial_seek(cursor);
        self.maybe_publish_segment_start(cursor);
        self.last_cursor = cursor;
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        self.initial_seek_published = true;
        let ReaderSeekSignal::Landed {
            landed_byte,
            preroll,
        } = signal
        else {
            return;
        };
        if let PrerollHint::Required(byte) = preroll {
            // TODO: route preroll byte to the coordinator once preroll plumbing lands.
            let _ = byte;
        }
        let Some(to) = landed_byte else {
            return;
        };
        let from = self.last_cursor;
        self.last_cursor = to;
        self.last_segment = None;
        self.publish_seek(from, to);
    }
}
