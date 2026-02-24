#![forbid(unsafe_code)]

use tokio::sync::broadcast;

use crate::Event;

/// Unified event bus for the kithara audio pipeline.
///
/// All components receive a cloned `EventBus` and publish events directly.
/// Subscribers receive all events from all components.
///
/// `publish()` is a sync call — works from both async tasks and blocking threads.
/// If there are no subscribers, events are silently dropped.
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Publish an event to all subscribers.
    ///
    /// Accepts any type that converts `Into<Event>`, so you can pass
    /// sub-enum values directly: `bus.publish(HlsEvent::EndOfStream)`.
    ///
    /// This is a sync call (no `.await`). Safe to call from blocking threads.
    pub fn publish<E: Into<Event>>(&self, event: E) {
        let _ = self.tx.send(event.into());
    }

    /// Subscribe to all future events.
    ///
    /// Each subscriber gets an independent receiver. Slow subscribers
    /// receive `RecvError::Lagged(n)` instead of blocking producers.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    use crate::FileEvent;

    fn assert_file_event(event: Event, expected: &FileEvent) {
        match event {
            Event::File(actual) => assert_eq!(&actual, expected),
            other => panic!("expected file event, got {other:?}"),
        }
    }

    #[kithara::test]
    fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.publish(FileEvent::EndOfStream);
    }

    #[kithara::test(tokio)]
    #[case(FileEvent::DownloadComplete { total_bytes: 42 })]
    #[case(FileEvent::EndOfStream)]
    async fn publish_and_subscribe(#[case] expected: FileEvent) {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(expected.clone());
        let event = rx.recv().await.unwrap();
        assert_file_event(event, &expected);
    }

    #[kithara::test(tokio)]
    #[case(FileEvent::EndOfStream)]
    #[case(FileEvent::DownloadError {
        error: "network".to_string()
    })]
    async fn multiple_subscribers_each_receive(#[case] expected: FileEvent) {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        bus.publish(expected.clone());
        assert_file_event(rx1.recv().await.unwrap(), &expected);
        assert_file_event(rx2.recv().await.unwrap(), &expected);
    }

    #[kithara::test(tokio)]
    async fn lagged_subscriber_gets_error() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for i in 0..10 {
            bus.publish(FileEvent::DownloadProgress {
                offset: i,
                total: None,
            });
        }
        let result = rx.recv().await;
        assert!(matches!(
            result,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }

    #[kithara::test]
    fn clone_shares_channel() {
        let bus1 = EventBus::new(16);
        let bus2 = bus1.clone();
        let mut rx = bus1.subscribe();
        bus2.publish(FileEvent::EndOfStream);
        assert!(rx.try_recv().is_ok());
    }
}
