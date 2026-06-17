use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::{DeferredBus, EventBus, FileEvent};
use kithara_stream::{ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};

use crate::coord::FileCoord;

/// Ring depth for the decode-core → shell event hand-off. A pass emits at
/// most one progress event per decoded chunk; this bounds the worst-case
/// post-seek skip burst without blocking the decode core.
const READER_EVENT_CAPACITY: usize = 256;

pub(crate) struct FileReaderEventSink {
    coord: Arc<FileCoord>,
    seek_epoch_handle: Arc<AtomicU64>,
    bus: DeferredBus<FileEvent>,
    initial_seek_published: bool,
    /// See `HlsReaderEventSink::initial_cursor` — same recreate-after-
    /// seek-failure scenario.
    initial_cursor: u64,
    last_cursor: u64,
}

impl FileReaderEventSink {
    pub(crate) fn new(
        bus: EventBus,
        coord: Arc<FileCoord>,
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
        }
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
        self.bus.enqueue(FileEvent::ReaderSeek {
            seek_epoch,
            from_offset: self.initial_cursor,
            to_offset: cursor,
        });
    }
}

impl ReaderEventSink for FileReaderEventSink {
    fn flush(&mut self) {
        self.bus.flush();
    }

    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        if !matches!(signal, ReaderChunkSignal::Chunk) {
            return;
        }
        let cursor = self.coord.position();
        self.publish_initial_seek(cursor);
        self.last_cursor = cursor;
        self.bus.enqueue(FileEvent::ReadProgress {
            position: cursor,
            total: self.coord.total_bytes(),
        });
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        self.initial_seek_published = true;
        let ReaderSeekSignal::Landed { landed_byte, .. } = signal else {
            return;
        };
        let Some(to) = landed_byte else {
            return;
        };
        let from = self.last_cursor;
        self.last_cursor = to;
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        self.bus.enqueue(FileEvent::ReaderSeek {
            seek_epoch,
            from_offset: from,
            to_offset: to,
        });
    }
}
