use kithara_events::{DeferredBus, EventBus};
use kithara_platform::sync::Arc;
use kithara_stream::{ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};

use crate::{FileReaderEvent, coord::FileCoord};

pub(crate) struct FileReaderEventSink {
    coord: Arc<FileCoord>,
    bus: DeferredBus<FileReaderEvent>,
    /// See `HlsReaderEventSink::initial_cursor` — same recreate-after-
    /// seek-failure scenario.
    last_cursor: u64,
}

impl FileReaderEventSink {
    pub(crate) fn new(bus: EventBus, coord: Arc<FileCoord>, event_capacity: usize) -> Self {
        let last_cursor = coord.position();
        Self {
            bus: DeferredBus::new(bus, event_capacity),
            coord,
            last_cursor,
        }
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
        self.last_cursor = cursor;
        self.bus.enqueue(FileReaderEvent::ReadProgress {
            position: cursor,
            total: self.coord.total_bytes(),
        });
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        let ReaderSeekSignal::Landed { landed_byte, .. } = signal else {
            return;
        };
        let Some(to) = landed_byte else {
            return;
        };
        let from = self.last_cursor;
        self.last_cursor = to;
        self.bus.enqueue(FileReaderEvent::ReaderSeek {
            from_offset: from,
            to_offset: to,
        });
    }
}
#[cfg(test)]
mod tests {
    use kithara_events::{BusEvent, DEFAULT_EVENT_BUS_CAPACITY, TryRecvError};
    use kithara_stream::PlayheadState;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{FileError, FileEvent};

    fn sink(bus: EventBus, event_capacity: usize) -> FileReaderEventSink {
        let coord = Arc::new(FileCoord::new(Arc::new(PlayheadState::new())));
        FileReaderEventSink::new(bus, coord, event_capacity)
    }

    fn burst(sink: &mut FileReaderEventSink, chunks: usize) {
        for _ in 0..chunks {
            sink.on_chunk(ReaderChunkSignal::Chunk);
        }
        sink.flush();
    }

    fn dropped_in(bus: &EventBus, chunks: usize, event_capacity: usize) -> u64 {
        let mut rx = bus.subscribe();
        burst(&mut sink(bus.clone(), event_capacity), chunks);
        let mut dropped = 0;
        while let Ok(envelope) = rx.try_recv() {
            if let BusEvent::Overflow { dropped: count, .. } = envelope.event {
                dropped += count;
            }
        }
        dropped
    }

    /// The ring depth is the caller's, not a crate constant: a one-slot ring
    /// keeps the first chunk of a burst and reports the rest as dropped.
    #[kithara::test]
    fn a_one_slot_ring_drops_the_rest_of_the_burst() {
        assert_eq!(dropped_in(&EventBus::new(16), 3, 1), 2);
    }

    /// The same burst through a ring deep enough to hold it drops nothing, so
    /// what the first test observes is the depth and not the burst.
    #[kithara::test]
    fn a_ring_deeper_than_the_burst_drops_nothing() {
        assert_eq!(dropped_in(&EventBus::new(16), 3, 4), 0);
    }

    #[kithara::test]
    fn reader_activity_does_not_displace_file_errors() {
        let bus = EventBus::default();
        let mut file_events = bus.subscribe::<FileEvent>();
        let mut reader_events = bus.subscribe::<FileReaderEvent>();
        let mut reader = sink(bus.clone(), 1);
        let before = FileEvent::Error {
            error: FileError::Io("before reader burst".into()),
        };
        let after = FileEvent::Error {
            error: FileError::Io("after reader burst".into()),
        };
        bus.publish(before.clone());
        for _ in 0..=DEFAULT_EVENT_BUS_CAPACITY {
            burst(&mut reader, 1);
        }
        bus.publish(after.clone());

        assert!(matches!(
            reader_events.try_recv(),
            Err(TryRecvError::Lagged(1))
        ));
        assert_eq!(file_events.try_recv().expect("first error").event, before);
        assert_eq!(file_events.try_recv().expect("second error").event, after);
        assert!(matches!(file_events.try_recv(), Err(TryRecvError::Empty)));
    }
}
