#![forbid(unsafe_code)]

use dashmap::DashMap;
use kithara_platform::{
    sync::{Arc, OnceLock},
    time::Instant,
    tokio::sync::broadcast,
};
use portable_atomic::{AtomicU64, Ordering};
use smallvec::SmallVec;

use crate::{
    Envelope, Event, EventMeta, ScopeLabel,
    scope::{BusScope, next_bus_id},
};

static EVENT_TIME_BASE: OnceLock<Instant> = OnceLock::new();

/// Default capacity for each per-scope broadcast channel.
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 1024;

/// Shared state for a bus hierarchy.
pub(crate) struct BusRegistry {
    topics: DashMap<u64, broadcast::Sender<Envelope>>,
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
    pub(crate) registry: Arc<BusRegistry>,
    pub(crate) scope: BusScope,
    pub(crate) label: ScopeLabel,
    next_seq: Arc<AtomicU64>,
    /// Cached senders for self + ancestors, matching scope path order.
    /// Eliminates `DashMap` lookups on the hot publish path.
    senders: SmallVec<[broadcast::Sender<Envelope>; 4]>,
}

impl EventBus {
    /// Create a root bus with the given channel capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        EVENT_TIME_BASE.get_or_init(Instant::now);
        let cap = capacity.max(1);
        let id = next_bus_id();
        let (tx, _) = broadcast::channel(cap);
        let next_seq = Arc::new(AtomicU64::new(0));
        let registry = Arc::new(BusRegistry {
            topics: DashMap::new(),
            capacity: cap,
        });
        registry.topics.insert(id, tx.clone());
        let mut senders = SmallVec::new();
        senders.push(tx);
        Self {
            registry,
            label: ScopeLabel::default(),
            next_seq,
            senders,
            scope: BusScope::root(id),
        }
    }

    /// This bus's unique id.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.scope.id()
    }

    /// Publish an event to this scope and all ancestor scopes.
    ///
    /// Accepts any type that converts `Into<Event>`, so you can pass
    /// sub-enum values directly: `bus.publish(HlsEvent::EndOfStream)`.
    pub fn publish<E: Into<Event>>(&self, event: E) {
        let event = Envelope {
            meta: EventMeta {
                origin: self.scope.id(),
                seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
                ts_micros: ts_micros(),
                deck: self.label.deck,
                track: self.label.track,
            },
            event: event.into(),
        };
        self.publish_envelope(event);
    }

    pub(crate) fn publish_envelope(&self, event: Envelope) {
        let len = self.senders.len();
        if len == 1 {
            self.senders[0].send(event).ok();
            return;
        }
        for sender in &self.senders[..len - 1] {
            sender.send(event.clone()).ok();
        }
        self.senders[len - 1].send(event).ok();
    }

    /// This bus's scope.
    #[must_use]
    pub fn scope(&self) -> &BusScope {
        &self.scope
    }

    pub(crate) fn next_seq_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.next_seq)
    }

    /// Create a child scope sharing the same topic registry.
    ///
    /// Events published to the child are visible to all ancestors.
    /// Subscribing to the child only receives events from its subtree.
    #[must_use]
    pub fn scoped(&self) -> Self {
        self.scoped_labeled(ScopeLabel::default())
    }

    /// Create a child scope with optional deck/track overrides.
    #[must_use]
    pub fn scoped_labeled(&self, label: ScopeLabel) -> Self {
        let id = next_bus_id();
        let (tx, _) = broadcast::channel(self.registry.capacity);
        self.registry.topics.insert(id, tx.clone());
        let scope = self.scope.child(id);
        let mut senders = SmallVec::with_capacity(self.senders.len() + 1);
        senders.push(tx);
        senders.extend(self.senders.iter().cloned());
        Self {
            scope,
            label: self.label.merged_with(label),
            next_seq: Arc::clone(&self.next_seq),
            senders,
            registry: Arc::clone(&self.registry),
        }
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
}

pub(crate) fn ts_micros() -> u64 {
    let micros = EVENT_TIME_BASE
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

impl Drop for EventBus {
    fn drop(&mut self) {
        let id = self.scope.id();
        if self.senders[0].receiver_count() == 0 {
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
    use crate::{BusEvent, FileError, FileEvent, SlotId, TrackId};

    #[cfg(feature = "file")]
    fn assert_file_event(event: &Envelope, expected: &FileEvent) {
        match &event.event {
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
    #[case(FileEvent::ReadProgress { position: 42, total: None })]
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
    #[case(FileEvent::Error {
        error: FileError::Io("network".to_string()),
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
            bus.publish(FileEvent::ReadProgress {
                position: i,
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
    #[case::root_sees_child(true)]
    #[case::child_sees_own(false)]
    async fn child_publish_is_visible_to_subscriber(#[case] subscribe_from_root: bool) {
        let root = EventBus::new(16);
        let child = root.scoped();
        let mut rx = if subscribe_from_root {
            root.subscribe()
        } else {
            child.subscribe()
        };

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
        child_b.publish(FileEvent::ReadProgress {
            position: 99,
            total: None,
        });

        let e1 = rx_root.recv().await.unwrap();
        let e2 = rx_root.recv().await.unwrap();
        assert_file_event(&e1, &FileEvent::EndOfStream);
        assert_file_event(
            &e2,
            &FileEvent::ReadProgress {
                position: 99,
                total: None,
            },
        );

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
        assert!(!root.registry.topics.contains_key(&child_id));
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn envelope_meta_stamps_scope_seq_and_time() {
        let root = EventBus::new(16);
        let child_a = root.scoped_labeled(ScopeLabel {
            track: Some(TrackId(7)),
            ..ScopeLabel::default()
        });
        let child_b = root.scoped();
        let mut rx = root.subscribe();

        child_a.publish(FileEvent::EndOfStream);
        child_b.publish(FileEvent::EndOfStream);

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert_ne!(first.meta.origin, second.meta.origin);
        assert_eq!(first.meta.track, Some(TrackId(7)));
        assert_eq!(second.meta.track, None);
        assert!(first.meta.seq < second.meta.seq);
        assert!(first.meta.ts_micros <= second.meta.ts_micros);
    }

    #[kithara::test]
    fn scoped_labeled_inherits_and_overrides() {
        let root = EventBus::new(16);
        let child = root.scoped_labeled(ScopeLabel {
            deck: Some(SlotId::new(1)),
            track: Some(TrackId(11)),
        });
        let grandchild = child.scoped_labeled(ScopeLabel {
            deck: None,
            track: Some(TrackId(12)),
        });

        assert_eq!(child.label.deck, Some(SlotId::new(1)));
        assert_eq!(child.label.track, Some(TrackId(11)));
        assert_eq!(grandchild.label.deck, Some(SlotId::new(1)));
        assert_eq!(grandchild.label.track, Some(TrackId(12)));
    }

    #[cfg(feature = "file")]
    #[kithara::test(tokio)]
    async fn overflow_event_publishes_exact_drop_count() {
        let bus = EventBus::new(16);
        let deferred = crate::DeferredBus::new(bus.clone(), 2);
        let mut rx = bus.subscribe();

        deferred.enqueue(FileEvent::EndOfStream);
        deferred.enqueue(FileEvent::EndOfStream);
        deferred.enqueue(FileEvent::EndOfStream);
        deferred.enqueue(FileEvent::EndOfStream);
        deferred.flush();

        let _ = rx.recv().await.unwrap();
        let _ = rx.recv().await.unwrap();
        let overflow = rx.recv().await.unwrap();
        match overflow.event {
            Event::Bus(BusEvent::Overflow { scope, dropped }) => {
                assert_eq!(scope, bus.id());
                assert_eq!(dropped, 2);
            }
            other => panic!("expected overflow event, got {other:?}"),
        }
    }
}
