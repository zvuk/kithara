#![forbid(unsafe_code)]

use kithara_platform::tokio::sync::broadcast;

use crate::{
    Event,
    receiver::EventReceiver,
    scope::{BusScope, next_bus_id},
};

/// Default capacity for event bus channels.
///
/// Sized for shared pipelines: large enough to absorb bursts
/// from multiple concurrent tracks without lagging subscribers.
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 1024;

/// Internal message on the broadcast channel.
///
/// Carries the event together with its publisher's scope so that
/// [`EventReceiver`] can filter by hierarchy.
#[derive(Clone, Debug)]
pub(crate) struct BusMessage {
    pub(crate) scope: BusScope,
    pub(crate) event: Event,
}

/// Hierarchical event bus for the kithara audio pipeline.
///
/// One root bus per player. Child scopes are created with [`scoped`](Self::scoped).
/// All scopes share the same broadcast channel; filtering happens at
/// the [`EventReceiver`] level.
///
/// `publish()` is a sync call — works from both async tasks and blocking threads.
/// If there are no subscribers, events are silently dropped.
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<BusMessage>,
    scope: BusScope,
}

impl EventBus {
    /// Create a root bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            scope: BusScope::root(next_bus_id()),
        }
    }

    /// Create a child scope sharing the same channel.
    ///
    /// Events published to the child are visible to all ancestors.
    /// Subscribing to the child only receives events from its subtree.
    #[must_use]
    pub fn scoped(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            scope: self.scope.child(next_bus_id()),
        }
    }

    /// Publish an event tagged with this bus's scope.
    ///
    /// Accepts any type that converts `Into<Event>`, so you can pass
    /// sub-enum values directly: `bus.publish(HlsEvent::EndOfStream)`.
    pub fn publish<E: Into<Event>>(&self, event: E) {
        let _ = self.tx.send(BusMessage {
            scope: self.scope.clone(),
            event: event.into(),
        });
    }

    /// Subscribe to events in this bus's scope.
    ///
    /// A root bus sees all events. A scoped bus sees only events
    /// published to itself or its descendants.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        EventReceiver::new(self.tx.subscribe(), self.scope.id())
    }

    /// This bus's unique id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.scope.id()
    }

    /// This bus's scope.
    #[must_use]
    pub fn scope(&self) -> &BusScope {
        &self.scope
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUS_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;
    #[cfg(feature = "file")]
    use crate::FileEvent;

    #[cfg(feature = "file")]
    fn assert_file_event(event: &Event, expected: &FileEvent) {
        match event {
            Event::File(actual) => assert_eq!(actual, expected),
            other => panic!("expected file event, got {other:?}"),
        }
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.publish(FileEvent::EndOfStream);
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    #[case(FileEvent::DownloadComplete { total_bytes: 42 })]
    #[case(FileEvent::EndOfStream)]
    async fn publish_and_subscribe(#[case] expected: FileEvent) {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(expected.clone());
        let event = rx.recv().await.unwrap();
        assert_file_event(&event, &expected);
    }

    #[cfg(feature = "file")]
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
        assert_file_event(&rx1.recv().await.unwrap(), &expected);
        assert_file_event(&rx2.recv().await.unwrap(), &expected);
    }

    #[cfg(feature = "file")]
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

    #[cfg(feature = "file")]
    #[kithara::test]
    fn clone_shares_channel() {
        let bus1 = EventBus::new(16);
        let bus2 = bus1.clone();
        let mut rx = bus1.subscribe();
        bus2.publish(FileEvent::EndOfStream);
        assert!(rx.try_recv().is_ok());
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn root_sees_child_events() {
        let root = EventBus::new(16);
        let child = root.scoped();
        let mut rx = root.subscribe();

        child.publish(FileEvent::EndOfStream);
        let event = rx.recv().await.unwrap();
        assert_file_event(&event, &FileEvent::EndOfStream);
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn child_does_not_see_sibling_events() {
        let root = EventBus::new(16);
        let child_a = root.scoped();
        let child_b = root.scoped();
        let mut rx_a = child_a.subscribe();

        child_b.publish(FileEvent::EndOfStream);

        // child_a should not receive child_b's event
        assert!(rx_a.try_recv().is_err());
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn child_sees_own_events() {
        let root = EventBus::new(16);
        let child = root.scoped();
        let mut rx = child.subscribe();

        child.publish(FileEvent::EndOfStream);
        let event = rx.recv().await.unwrap();
        assert_file_event(&event, &FileEvent::EndOfStream);
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn root_sees_grandchild_events() {
        let root = EventBus::new(16);
        let child = root.scoped();
        let grandchild = child.scoped();
        let mut rx = root.subscribe();

        grandchild.publish(FileEvent::EndOfStream);
        let event = rx.recv().await.unwrap();
        assert_file_event(&event, &FileEvent::EndOfStream);
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn parent_sees_child_but_not_sibling() {
        let root = EventBus::new(16);
        let child_a = root.scoped();
        let child_b = root.scoped();
        let mut rx_root = root.subscribe();
        let mut rx_a = child_a.subscribe();

        child_a.publish(FileEvent::EndOfStream);
        child_b.publish(FileEvent::DownloadComplete { total_bytes: 99 });

        // Root sees both
        let e1 = rx_root.recv().await.unwrap();
        let e2 = rx_root.recv().await.unwrap();
        assert_file_event(&e1, &FileEvent::EndOfStream);
        assert_file_event(&e2, &FileEvent::DownloadComplete { total_bytes: 99 });

        // child_a only sees its own
        let ea = rx_a.recv().await.unwrap();
        assert_file_event(&ea, &FileEvent::EndOfStream);
        assert!(rx_a.try_recv().is_err());
    }

    #[kithara::test]
    fn scoped_gets_unique_id() {
        let root = EventBus::new(16);
        let c1 = root.scoped();
        let c2 = root.scoped();
        assert_ne!(root.id(), c1.id());
        assert_ne!(root.id(), c2.id());
        assert_ne!(c1.id(), c2.id());
    }

    #[kithara::test]
    fn default_uses_large_capacity() {
        let bus = EventBus::default();
        // Just verify it doesn't panic and has a valid id
        assert!(bus.id() > 0);
    }
}
