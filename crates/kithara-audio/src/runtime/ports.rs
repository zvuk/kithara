use kithara_platform::sync::Arc;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};

/// A signal to wake up a blocked consumer.
pub(crate) trait WakeSignal: Send + Sync + 'static {
    /// Flush any wake-side deferred signals from the scheduler shell.
    fn flush_deferred(&self) {}

    /// Signal that the ring went from empty to non-empty. Fired only on that
    /// transition so an event-driven (non-blocking) reader gets one wake hint
    /// per drain cycle without flooding the shared event bus. Default no-op.
    fn on_data_available(&self) {}

    /// Wake up the consumer. Fired on every successful ring push so a reader
    /// parked in a blocking `recv` is unparked whenever new output lands —
    /// redundant unparks are cheap and close the park/push race.
    fn wake(&self);
}

/// The output port of a node.
///
/// Backed by a single-producer/single-consumer ring buffer plus a one-slot
/// overflow used to absorb a single backpressure miss. Producers that
/// guarantee at most one push per tick can therefore treat [`try_push`] as
/// infallible: an item that does not fit into the ring is parked in the
/// overflow slot and forwarded on the next [`try_push`] / [`flush`] once
/// the consumer drains the ring.
///
/// [`try_push`]: Outlet::try_push
/// [`flush`]: Outlet::flush
pub(crate) struct Outlet<T> {
    producer: HeapProd<T>,
    overflow: Option<T>,
    wake: Option<Arc<dyn WakeSignal>>,
}

impl<T> Outlet<T> {
    /// Try to drain the parked overflow item into the ring buffer.
    ///
    /// Returns `true` if the overflow slot is empty after the call (either
    /// because there was nothing parked, or because the parked item was
    /// successfully forwarded). Returns `false` when the ring buffer is
    /// still full and the parked item could not be moved.
    pub(crate) fn flush(&mut self) -> bool {
        let Some(item) = self.overflow.take() else {
            return true;
        };
        self.push_or_park(item)
    }

    /// Flush any deferred work owned by the wake signal.
    pub(crate) fn flush_wake_signals(&self) {
        if let Some(wake) = &self.wake {
            wake.flush_deferred();
        }
    }

    delegate::delegate! {
        to self.overflow {
            /// Whether an item is currently parked in the overflow slot.
            #[call(is_some)]
            pub (crate) fn has_pending (& self) -> bool;
            /// Discard the parked overflow item, returning it to the caller.
            ///
            /// Useful when a producer needs to invalidate previously enqueued data
            /// (e.g. on a seek epoch change) without waiting for the consumer to
            /// drain the ring.
            #[call(take)]
            pub (crate) fn take_pending (& mut self) -> Option < T >;
        }
    }
    /// Whether both the ring buffer and the overflow slot are full.
    ///
    /// When `true`, the next [`try_push`](Self::try_push) is guaranteed to
    /// return `Err`.
    #[cfg(test)]
    pub(crate) fn is_full(&self) -> bool {
        self.overflow.is_some() && self.producer.is_full()
    }

    fn notify(&self) {
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    fn notify_data_available(&self) {
        if let Some(wake) = &self.wake {
            wake.on_data_available();
        }
    }

    /// Try to push into the ring; on failure, park into the (assumed empty)
    /// overflow slot. Returns `true` when the item reached the ring, `false`
    /// when it was parked. A successful push always `wake`s a blocking reader;
    /// the empty-to-non-empty transition additionally fires the
    /// `on_data_available` hint for event-driven readers.
    fn push_or_park(&mut self, item: T) -> bool {
        debug_assert!(
            self.overflow.is_none(),
            "push_or_park called with non-empty overflow"
        );
        let was_empty = self.producer.is_empty();
        match self.producer.try_push(item) {
            Ok(()) => {
                self.notify();
                if was_empty {
                    self.notify_data_available();
                }
                true
            }
            Err(item) => {
                self.overflow = Some(item);
                false
            }
        }
    }

    /// Push an item into the outlet.
    ///
    /// First tries to drain the overflow slot, then attempts to push `item`
    /// into the ring buffer. If the ring is full but the overflow slot is
    /// empty, `item` is parked there and `Ok(())` is returned. `Err(item)`
    /// is returned only when both the ring and the overflow slot are
    /// saturated.
    ///
    /// # Errors
    ///
    /// Returns `Err(item)` when the ring and overflow slot are both full.
    pub(crate) fn try_push(&mut self, item: T) -> Result<(), T> {
        if !self.flush() {
            return Err(item);
        }
        let _ = self.push_or_park(item);
        Ok(())
    }
}

/// The input port of a node.
pub(crate) struct Inlet<T> {
    consumer: HeapCons<T>,
}

impl<T> Inlet<T> {
    delegate::delegate! {
        to self.consumer {
            /// Check if the inlet is empty.
            #[cfg(test)]
            pub (crate) fn is_empty (& self) -> bool;
            /// Pop an item from the inlet. Returns `None` if empty.
            pub (crate) fn try_pop (& mut self) -> Option < T >;
        }
    }
}

/// Connect two nodes with a lock-free ring buffer.
#[must_use]
pub(crate) fn connect<T>(
    capacity: usize,
    wake: Option<Arc<dyn WakeSignal>>,
) -> (Outlet<T>, Inlet<T>) {
    let rb = HeapRb::<T>::new(capacity);
    let (producer, consumer) = rb.split();
    (
        Outlet {
            producer,
            wake,
            overflow: None,
        },
        Inlet { consumer },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use kithara_test_utils::kithara;

    use super::*;

    struct TestWake {
        woken: AtomicBool,
    }

    impl WakeSignal for TestWake {
        fn wake(&self) {
            self.woken.store(true, Ordering::SeqCst);
        }
    }

    struct CountingWake {
        count: AtomicUsize,
        data_available: AtomicUsize,
    }

    impl WakeSignal for CountingWake {
        fn on_data_available(&self) {
            self.data_available.fetch_add(1, Ordering::SeqCst);
        }

        fn wake(&self) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[kithara::test]
    fn connect_push_pop() {
        let (mut out, mut inl) = connect::<i32>(2, None);
        assert!(inl.is_empty());
        assert!(!out.is_full());

        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(out.try_push(2), Ok(()));
        assert_eq!(out.try_push(3), Ok(()));
        assert!(out.has_pending());
        assert_eq!(out.try_push(4), Err(4));
        assert!(out.is_full());

        assert_eq!(inl.try_pop(), Some(1));
        assert_eq!(inl.try_pop(), Some(2));
        assert_eq!(inl.try_pop(), None);

        assert!(out.flush());
        assert!(!out.has_pending());
        assert_eq!(inl.try_pop(), Some(3));
        assert_eq!(inl.try_pop(), None);
        assert!(inl.is_empty());
    }

    #[kithara::test]
    fn try_push_drains_overflow_first() {
        let (mut out, mut inl) = connect::<i32>(1, None);

        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(out.try_push(2), Ok(()));
        assert!(out.has_pending());

        assert_eq!(inl.try_pop(), Some(1));
        assert_eq!(out.try_push(3), Ok(()));
        assert!(out.has_pending());

        assert_eq!(inl.try_pop(), Some(2));
        assert!(out.flush());
        assert_eq!(inl.try_pop(), Some(3));
    }

    #[kithara::test]
    fn flush_returns_false_when_ring_full() {
        let (mut out, mut inl) = connect::<i32>(1, None);

        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(out.try_push(2), Ok(()));
        assert!(out.has_pending());

        assert!(!out.flush());
        assert!(out.has_pending());

        assert_eq!(inl.try_pop(), Some(1));
        assert!(out.flush());
        assert!(!out.has_pending());
    }

    #[kithara::test]
    fn take_pending_clears_overflow() {
        let (mut out, _inl) = connect::<i32>(1, None);

        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(out.try_push(2), Ok(()));
        assert_eq!(out.take_pending(), Some(2));
        assert!(!out.has_pending());
        assert_eq!(out.take_pending(), None);
    }

    #[kithara::test]
    fn wake_signal() {
        let wake = Arc::new(TestWake {
            woken: AtomicBool::new(false),
        });
        let (mut out, _inl) = connect::<i32>(2, Some(wake.clone()));

        assert!(!wake.woken.load(Ordering::SeqCst));
        assert_eq!(out.try_push(42), Ok(()));
        assert!(wake.woken.load(Ordering::SeqCst));
    }

    #[kithara::test]
    fn wake_skipped_when_parking_in_overflow() {
        let wake = Arc::new(CountingWake {
            count: AtomicUsize::new(0),
            data_available: AtomicUsize::new(0),
        });
        let (mut out, mut inl) = connect::<i32>(1, Some(wake.clone()));

        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(wake.count.load(Ordering::SeqCst), 1);

        // Parks in overflow (ring full): no ring push, so no wake.
        assert_eq!(out.try_push(2), Ok(()));
        assert_eq!(wake.count.load(Ordering::SeqCst), 1);

        assert_eq!(inl.try_pop(), Some(1));
        assert!(out.flush());
        assert_eq!(wake.count.load(Ordering::SeqCst), 2);
    }

    #[kithara::test]
    fn wake_fires_every_ring_push_data_available_only_on_transition() {
        let wake = Arc::new(CountingWake {
            count: AtomicUsize::new(0),
            data_available: AtomicUsize::new(0),
        });
        let (mut out, mut inl) = connect::<i32>(2, Some(wake.clone()));

        // Empty -> non-empty: wake (unpark) AND the data-available hint.
        assert_eq!(out.try_push(1), Ok(()));
        assert_eq!(wake.count.load(Ordering::SeqCst), 1);
        assert_eq!(wake.data_available.load(Ordering::SeqCst), 1);

        // Push onto a non-empty ring: still wakes a blocking reader, but no
        // duplicate data-available hint for an event-driven reader.
        assert_eq!(out.try_push(2), Ok(()));
        assert_eq!(wake.count.load(Ordering::SeqCst), 2);
        assert_eq!(
            wake.data_available.load(Ordering::SeqCst),
            1,
            "data-available hint fires only on the empty-to-non-empty transition"
        );

        assert_eq!(inl.try_pop(), Some(1));
        assert_eq!(inl.try_pop(), Some(2));
        // Drained to empty, next push is a fresh transition.
        assert_eq!(out.try_push(3), Ok(()));
        assert_eq!(wake.count.load(Ordering::SeqCst), 3);
        assert_eq!(wake.data_available.load(Ordering::SeqCst), 2);
    }
}
