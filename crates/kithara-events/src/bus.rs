#![forbid(unsafe_code)]

use std::sync::Arc;

use dashmap::DashMap;
use kithara_platform::tokio::sync::broadcast;
use smallvec::SmallVec;

use crate::{
    Event,
    scope::{BusScope, next_bus_id},
};

/// Default capacity for each per-scope broadcast channel.
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 1024;

/// Shared state for a bus hierarchy.
struct BusRegistry {
    topics: DashMap<u64, broadcast::Sender<Event>>,
    capacity: usize,
}

/// Hierarchical event bus for the kithara audio pipeline.
///
/// One root bus per player. Child scopes are created with [`scoped`](Self::scoped).
/// Each scope has its own broadcast channel. Publishing sends to the
/// publisher's channel and all ancestor channels (topic-based routing).
/// Subscribers only receive events from their scope's subtree — zero
/// wasted recv/filter work.
#[derive(Clone)]
pub struct EventBus {
    registry: Arc<BusRegistry>,
    scope: BusScope,
    /// Cached senders for self + ancestors, matching scope path order.
    /// Eliminates `DashMap` lookups on the hot publish path.
    senders: SmallVec<[broadcast::Sender<Event>; 4]>,
}

impl EventBus {
    /// Create a root bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let id = next_bus_id();
        let (tx, _) = broadcast::channel(cap);
        let registry = Arc::new(BusRegistry {
            topics: DashMap::new(),
            capacity: cap,
        });
        registry.topics.insert(id, tx.clone());
        let mut senders = SmallVec::new();
        senders.push(tx);
        Self {
            registry,
            scope: BusScope::root(id),
            senders,
        }
    }

    /// Create a child scope sharing the same topic registry.
    ///
    /// Events published to the child are visible to all ancestors.
    /// Subscribing to the child only receives events from its subtree.
    #[must_use]
    pub fn scoped(&self) -> Self {
        let id = next_bus_id();
        let (tx, _) = broadcast::channel(self.registry.capacity);
        self.registry.topics.insert(id, tx.clone());
        let scope = self.scope.child(id);
        // Cache senders: self (new) + ancestors (from parent's cache).
        let mut senders = SmallVec::with_capacity(self.senders.len() + 1);
        senders.push(tx);
        senders.extend(self.senders.iter().cloned());
        Self {
            registry: Arc::clone(&self.registry),
            scope,
            senders,
        }
    }

    /// Publish an event to this scope and all ancestor scopes.
    ///
    /// Accepts any type that converts `Into<Event>`, so you can pass
    /// sub-enum values directly: `bus.publish(HlsEvent::EndOfStream)`.
    pub fn publish<E: Into<Event>>(&self, event: E) {
        let event = event.into();
        let len = self.senders.len();
        if len == 1 {
            let _ = self.senders[0].send(event);
            return;
        }
        // Clone for all but last, consume owned event on last send.
        for sender in &self.senders[..len - 1] {
            let _ = sender.send(event.clone());
        }
        let _ = self.senders[len - 1].send(event);
    }

    /// Subscribe to events in this bus's scope.
    ///
    /// A root bus sees all events (its own + all descendants).
    /// A scoped bus sees only events published to itself or its descendants.
    /// No filtering overhead — each scope has a dedicated channel.
    #[must_use]
    pub fn subscribe(&self) -> crate::EventReceiver {
        crate::EventReceiver::new(self.senders[0].subscribe())
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

impl Drop for EventBus {
    fn drop(&mut self) {
        // Only remove the topic if this is the last EventBus holding a sender
        // for this scope. broadcast::Sender strong_count includes the one in
        // DashMap + one per EventBus clone that has it cached. When our cached
        // sender is the last clone besides the DashMap entry (count == 2),
        // removing is safe. For root/ancestor senders cached by children,
        // strong_count will be higher, so we don't remove them.
        let id = self.scope.id();
        if self.senders[0].receiver_count() == 0 {
            // No subscribers and we are being dropped — clean up.
            self.registry.topics.remove(&id);
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUS_CAPACITY)
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("id", &self.scope.id())
            .field("depth", &self.scope.depth())
            .finish_non_exhaustive()
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

        let e1 = rx_root.recv().await.unwrap();
        let e2 = rx_root.recv().await.unwrap();
        assert_file_event(&e1, &FileEvent::EndOfStream);
        assert_file_event(&e2, &FileEvent::DownloadComplete { total_bytes: 99 });

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
    fn default_creates_valid_bus() {
        let bus = EventBus::default();
        assert!(bus.id() > 0);
    }

    #[cfg(feature = "file")]
    #[kithara::test]
    fn dropped_scoped_bus_cleans_up_topic() {
        let root = EventBus::new(16);
        let child_id;
        {
            let child = root.scoped();
            child_id = child.id();
            assert!(root.registry.topics.contains_key(&child_id));
        }
        // After drop, topic entry should be removed (no subscribers).
        assert!(!root.registry.topics.contains_key(&child_id));
    }
}
