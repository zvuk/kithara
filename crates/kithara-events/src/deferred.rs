use crossbeam_queue::ArrayQueue;
use portable_atomic::{AtomicU64, Ordering};

use crate::{BusEvent, Envelope, Event, EventBus, EventMeta};

/// Decode-core → shell hand-off for reader-hook events.
///
/// Reader hooks run on the worker's forbid-blocking decode core, where they
/// resolve a fully-formed event from live cursor state. Publishing it goes
/// through `tokio::broadcast::Sender::send`, which takes an internal lock —
/// forbidden on the produce core. `DeferredBus` splits the two:
/// [`enqueue`](Self::enqueue) pushes the resolved event into a fixed,
/// lock-free ring on the decode core (no alloc, no lock); [`flush`](Self::flush)
/// drains the ring and publishes from the scheduler's unchecked shell, once
/// per pass. The ring is FIFO, so events keep their decode order.
///
/// The element is the narrow per-domain event (`HlsEvent` / `FileEvent`),
/// converted to [`Event`] only at publish time, so the ring stays small.
pub struct DeferredBus<E> {
    pending: ArrayQueue<DeferredEvent<E>>,
    bus: EventBus,
    next_seq: kithara_platform::sync::Arc<AtomicU64>,
    dropped: AtomicU64,
}

struct DeferredEvent<E> {
    seq: u64,
    ts_micros: u64,
    event: E,
}

impl<E: Into<Event>> DeferredBus<E> {
    /// The underlying scoped bus this ring flushes into.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Build a deferred sink over `bus` with a fixed ring of `capacity`
    /// slots. `capacity` is clamped to at least one.
    #[must_use]
    pub fn new(bus: EventBus, capacity: usize) -> Self {
        let next_seq = bus.next_seq_counter();
        Self {
            bus,
            next_seq,
            dropped: AtomicU64::new(0),
            pending: ArrayQueue::new(capacity.max(1)),
        }
    }

    /// Queue a resolved event for shell-side publish.
    ///
    /// Lock-free and alloc-free, so it is safe to call from the decode core.
    /// Drops the event if the ring is full: the only high-volume producer is
    /// monotonic progress, where the next pass's event supersedes a dropped
    /// one, so a drop under burst is self-healing.
    pub fn enqueue(&self, event: E) {
        let pending = DeferredEvent {
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            ts_micros: crate::bus::ts_micros(),
            event,
        };
        if self.pending.push(pending).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drain the ring and publish every queued event in FIFO order.
    ///
    /// Runs in the unchecked scheduler shell, so the `broadcast::send` lock
    /// stays off the forbid-blocking decode core.
    pub fn flush(&self) {
        while let Some(event) = self.pending.pop() {
            self.bus.publish_envelope(Envelope {
                meta: EventMeta {
                    origin: self.bus.scope.id(),
                    seq: event.seq,
                    ts_micros: event.ts_micros,
                    deck: self.bus.label.deck,
                    track: self.bus.label.track,
                },
                event: event.event.into(),
            });
        }
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.bus.publish(BusEvent::Overflow {
                scope: self.bus.scope.id(),
                dropped,
            });
        }
    }
}

#[cfg(all(test, feature = "file"))]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{BusEvent, EventMeta, FileEvent};

    fn progress(position: u64) -> FileEvent {
        FileEvent::ReadProgress {
            position,
            total: None,
        }
    }

    #[track_caller]
    fn assert_progress(event: &Envelope, position: u64) {
        match &event.event {
            Event::File(actual) => assert_eq!(*actual, progress(position)),
            other => panic!("expected file progress event, got {other:?}"),
        }
    }

    #[kithara::test(tokio)]
    async fn enqueue_holds_until_flush() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let deferred = DeferredBus::new(bus.clone(), 8);

        deferred.enqueue(progress(1));
        deferred.enqueue(progress(2));

        assert!(
            rx.try_recv().is_err(),
            "enqueue must not publish on the decode core"
        );

        deferred.flush();

        assert_progress(&rx.recv().await.unwrap(), 1);
        assert_progress(&rx.recv().await.unwrap(), 2);
        assert!(rx.try_recv().is_err(), "flush drains the ring exactly once");
    }

    #[kithara::test]
    fn enqueue_drops_when_full_without_blocking() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let deferred = DeferredBus::new(bus.clone(), 2);

        deferred.enqueue(progress(1));
        deferred.enqueue(progress(2));
        // Ring full — the third enqueue drops rather than blocking or growing.
        deferred.enqueue(progress(3));

        deferred.flush();

        assert_progress(&rx.try_recv().unwrap(), 1);
        assert_progress(&rx.try_recv().unwrap(), 2);
        let overflow = rx.try_recv().unwrap();
        match overflow.event {
            Event::Bus(BusEvent::Overflow { dropped, .. }) => assert_eq!(dropped, 1),
            other => panic!("expected overflow event, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "earlier events survive in order");
    }

    #[kithara::test(tokio)]
    async fn flush_preserves_enqueue_time_seq_and_ts() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let deferred = DeferredBus::new(bus, 4);

        deferred.enqueue(progress(1));
        deferred.enqueue(progress(2));
        deferred.flush();

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        let EventMeta {
            seq: first_seq,
            ts_micros: first_ts,
            ..
        } = first.meta;
        let EventMeta {
            seq: second_seq,
            ts_micros: second_ts,
            ..
        } = second.meta;
        assert!(first_seq < second_seq);
        assert!(first_ts <= second_ts);
    }
}
