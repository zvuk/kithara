//! File-side `DecoderHooks` implementation.
//!
//! Emits `FileEvent::ReadProgress` once per chunk and
//! `FileEvent::ReaderSeek` once per `Decoder::seek`. Mirrors the
//! HLS hooks but without segment-level bookkeeping (file streams are
//! a single byte sequence).

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use kithara_events::{EventBus, FileEvent};
use kithara_stream::{DecoderHooks, ReaderChunkSignal, ReaderSeekSignal};

use crate::coord::FileCoord;

pub(crate) struct FileReaderHooks {
    byte_cursor: Arc<AtomicU64>,
    coord: Arc<FileCoord>,
    seek_epoch_handle: Arc<AtomicU64>,
    bus: EventBus,
    initial_seek_published: bool,
    /// See `HlsReaderHooks::initial_cursor` — same recreate-after-
    /// seek-failure scenario.
    initial_cursor: u64,
    last_cursor: u64,
}

impl FileReaderHooks {
    pub(crate) fn new(
        bus: EventBus,
        coord: Arc<FileCoord>,
        byte_cursor: Arc<AtomicU64>,
        seek_epoch_handle: Arc<AtomicU64>,
    ) -> Self {
        let last_cursor = byte_cursor.load(Ordering::Relaxed);
        Self {
            bus,
            byte_cursor,
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
        self.bus.publish(FileEvent::ReaderSeek {
            seek_epoch,
            from_offset: self.initial_cursor,
            to_offset: cursor,
        });
    }
}

impl DecoderHooks for FileReaderHooks {
    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        if !matches!(signal, ReaderChunkSignal::Chunk) {
            return;
        }
        let cursor = self.byte_cursor.load(Ordering::Relaxed);
        self.publish_initial_seek(cursor);
        self.last_cursor = cursor;
        self.bus.publish(FileEvent::ReadProgress {
            position: cursor,
            total: self.coord.total_bytes(),
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
        self.last_cursor = to;
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        self.bus.publish(FileEvent::ReaderSeek {
            seek_epoch,
            from_offset: from,
            to_offset: to,
        });
    }
}
