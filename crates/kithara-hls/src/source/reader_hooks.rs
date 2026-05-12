use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::{EventBus, HlsEvent};
use kithara_platform::Mutex;
use kithara_stream::{DecoderHooks, ReaderChunkSignal, ReaderSeekSignal};

use crate::{
    ids::{SegmentIndex, VariantIndex},
    stream_index::StreamIndex,
};

/// Hook implementation. Constructed by
/// [`HlsSource::take_reader_hooks`](super::core::HlsSource::take_reader_hooks)
/// and handed off to the audio pipeline at decoder-create time.
pub(crate) struct HlsReaderHooks {
    /// Shared coord — owner of the source's byte cursor; we poll
    /// `coord.position()` on each chunk without holding `&Source`.
    coord: Arc<crate::coord::HlsCoord>,
    /// Shared atomic seek epoch — read into the `ReaderSeek` payload
    /// for cross-event correlation.
    seek_epoch_handle: Arc<AtomicU64>,
    segments: Arc<Mutex<StreamIndex>>,
    bus: EventBus,
    /// `(variant, segment_index, bytes_accumulated_in_this_segment)`
    /// for the segment the reader is currently inside. `None` until
    /// the first chunk is observed (or after a `ReaderSeek` reset).
    state: Option<(VariantIndex, SegmentIndex, u64)>,
    /// Have we already published the construction-time `ReaderSeek`?
    initial_seek_published: bool,
    /// Cursor at construction time. The first successful `on_chunk`
    /// uses this to synthesise a `ReaderSeek` event — a fresh
    /// `HlsReaderHooks` instance handed off to the audio pipeline
    /// after a decoder recreate (e.g. seek-with-recreate path) is the
    /// only signal we have that the reader has jumped, since
    /// `inner.seek(...)` returned `Err(Interrupted)` and never
    /// produced a `DecoderSeekOutcome`.
    initial_cursor: u64,
    /// Cursor value at the previous `on_chunk` invocation. Used to
    /// derive `bytes_read` per chunk via cursor delta — backends only
    /// hand us a chunk signal, not a byte count.
    last_cursor: u64,
}

impl HlsReaderHooks {
    pub(crate) fn new(
        bus: EventBus,
        segments: Arc<Mutex<StreamIndex>>,
        coord: Arc<crate::coord::HlsCoord>,
        seek_epoch_handle: Arc<AtomicU64>,
    ) -> Self {
        let last_cursor = coord.position();
        Self {
            bus,
            segments,
            coord,
            seek_epoch_handle,
            state: None,
            last_cursor,
            initial_cursor: last_cursor,
            initial_seek_published: false,
        }
    }

    /// Publish a `ReaderSeek` event from the cursor at construction
    /// time. Called once on the first successful `on_chunk` so that
    /// the seek-with-recreate path (where `inner.seek` returns
    /// `Interrupted` and the audio pipeline rebuilds the decoder)
    /// still produces an observable seek signal.
    /// Publish a `ReaderSeek` event on the first successful chunk
    /// observed by this hook instance. `cursor` is the byte position
    /// **after** that first chunk was produced — by the time we land
    /// here the decoder has already pumped at least one read through
    /// the stream, so the cursor reflects the new (post-seek-recreate)
    /// position. `seek_epoch > 0` is used as the discriminator
    /// between "fresh open" (epoch 0, no seek happened, skip event)
    /// and "post-seek recreate" (epoch ≥ 1, publish event).
    fn publish_initial_seek(&mut self, cursor: u64) {
        if self.initial_seek_published {
            return;
        }
        self.initial_seek_published = true;
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        if seek_epoch == 0 {
            return;
        }
        let resolved = self.resolve(cursor);
        self.bus.publish(HlsEvent::ReaderSeek {
            seek_epoch,
            from_offset: self.initial_cursor,
            to_offset: cursor,
            variant: resolved.map(|(v, _, _)| v),
            segment_index: resolved.map(|(_, s, _)| s),
            byte_in_segment: resolved.map(|(_, _, b)| b),
        });
    }

    /// Resolve `offset` to `(variant, segment_index, byte_in_segment)`
    /// using the active layout variant. Returns `None` for offsets
    /// outside any committed segment (e.g. far-end seek before the
    /// segment is loaded).
    fn resolve(&self, offset: u64) -> Option<(VariantIndex, SegmentIndex, u64)> {
        let snapshot = {
            let segs = self.segments.lock_sync();
            segs.find_at_offset(offset)
                .map(|s| (s.variant, s.segment_index, s.byte_offset))
        };
        let (variant, segment_index, byte_offset) = snapshot?;
        Some((variant, segment_index, offset.saturating_sub(byte_offset)))
    }
}

impl DecoderHooks for HlsReaderHooks {
    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        if !matches!(signal, ReaderChunkSignal::Chunk) {
            return;
        }
        let cursor = self.coord.position();
        self.publish_initial_seek(cursor);
        let delta = cursor.saturating_sub(self.last_cursor);
        self.last_cursor = cursor;

        let resolve_at = cursor.saturating_sub(1);
        let current = self.resolve(resolve_at);

        match (self.state, current) {
            (Some((pv, ps, prev_bytes)), Some((cv, cs, _))) if (pv, ps) == (cv, cs) => {
                self.state = Some((pv, ps, prev_bytes.saturating_add(delta)));
            }
            (prev, current) => {
                if let Some((pv, ps, pb)) = prev {
                    self.bus.publish(HlsEvent::SegmentReadComplete {
                        variant: pv,
                        segment_index: ps,
                        bytes_read: pb,
                    });
                }
                if let Some((cv, cs, byte_in_seg)) = current {
                    self.bus.publish(HlsEvent::SegmentReadStart {
                        variant: cv,
                        segment_index: cs,
                        byte_offset: cursor.saturating_sub(byte_in_seg),
                    });
                    self.state = Some((cv, cs, delta));
                } else {
                    self.state = None;
                }
            }
        }

        let total = self.segments.lock_sync().max_end_offset();
        self.bus.publish(HlsEvent::ReadProgress {
            position: cursor,
            total: Some(total),
        });
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        self.initial_seek_published = true;
        let ReaderSeekSignal::Landed { landed_byte } = signal else {
            return;
        };
        let Some(to) = landed_byte else {
            return;
        };

        let from = self.last_cursor;
        self.state = None;
        self.last_cursor = to;

        let resolved = self.resolve(to);
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        self.bus.publish(HlsEvent::ReaderSeek {
            seek_epoch,
            from_offset: from,
            to_offset: to,
            variant: resolved.map(|(v, _, _)| v),
            segment_index: resolved.map(|(_, s, _)| s),
            byte_in_segment: resolved.map(|(_, _, b)| b),
        });
    }
}
