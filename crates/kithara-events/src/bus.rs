#![forbid(unsafe_code)]

use kithara_platform::{
    sync::{Arc, OnceLock},
    time::Instant,
    tokio::sync::broadcast,
};
use portable_atomic::{AtomicU64, Ordering};
use smallvec::SmallVec;

use crate::{
    Envelope, Event, EventMeta, EventReceiver, EventSet, ScopeLabel,
    scope::{BusScope, next_bus_id},
    topic::ScopeTopics,
};

static EVENT_TIME_BASE: OnceLock<Instant> = OnceLock::new();

/// Default capacity for each per-scope broadcast channel.
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 1024;

/// Hierarchical bus with one channel per scope and event type.
#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct EventBus {
    #[field(get)]
    pub(crate) scope: BusScope,
    pub(crate) label: ScopeLabel,
    next_seq: Arc<AtomicU64>,
    topics: SmallVec<[Arc<ScopeTopics>; 4]>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        EVENT_TIME_BASE.get_or_init(Instant::now);
        let mut topics = SmallVec::new();
        topics.push(Arc::new(ScopeTopics::new(capacity.max(1))));
        Self {
            scope: BusScope::root(next_bus_id()),
            label: ScopeLabel::default(),
            next_seq: Arc::new(AtomicU64::new(0)),
            topics,
        }
    }

    #[must_use]
    pub fn scoped(&self) -> Self {
        self.scoped_labeled(ScopeLabel::default())
    }

    pub fn scoped_labeled(&self, label: ScopeLabel) -> Self {
        let mut topics = SmallVec::with_capacity(self.topics.len() + 1);
        topics.push(Arc::new(ScopeTopics::new(self.topics[0].capacity())));
        topics.extend(self.topics.iter().map(Arc::clone));
        Self {
            scope: self.scope.child(next_bus_id()),
            label: self.label.merged_with(label),
            next_seq: Arc::clone(&self.next_seq),
            topics,
        }
    }

    #[must_use]
    pub fn id(&self) -> u64 {
        self.scope.id()
    }

    pub(crate) fn next_seq_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.next_seq)
    }

    pub(crate) fn meta(&self, seq: u64, ts_micros: u64) -> EventMeta {
        EventMeta {
            origin: self.scope.id(),
            seq,
            ts_micros,
            deck: self.label.deck,
            track: self.label.track,
        }
    }

    /// Stamps `event` and sends it to every scope on this bus's path that has a
    /// subscriber for its type.
    pub fn publish<S: EventSet>(&self, event: S) {
        let meta = self.meta(self.next_seq.fetch_add(1, Ordering::Relaxed), ts_micros());
        S::publish(self, meta, event);
    }

    /// Sends an already-stamped event of one concrete type.
    ///
    /// This is the seam `EventSet::publish` and [`DeferredBus::flush`] use to
    /// re-publish an event under a sequence number taken earlier.
    pub fn publish_stamped<E: Event>(&self, meta: EventMeta, event: E) {
        let mut targets = self.topics.iter().filter_map(|scope| scope.find::<E>());
        let Some(mut current) = targets.next() else {
            return;
        };
        let envelope = Envelope { event, meta };
        for next in targets {
            current.send(envelope.clone());
            current = next;
        }
        current.send(envelope);
    }

    #[must_use]
    pub fn subscribe<S: EventSet>(&self) -> EventReceiver<S> {
        EventReceiver::new(S::subscribe(self))
    }

    pub(crate) fn subscribe_topic<E: Event>(&self) -> broadcast::Receiver<Envelope<E>> {
        self.topics[0].find_or_insert::<E>().subscribe()
    }
}

pub(crate) fn ts_micros() -> u64 {
    let micros = EVENT_TIME_BASE
        .get_or_init(Instant::now)
        .elapsed()
        .as_micros();
    u64::try_from(micros).unwrap_or(u64::MAX)
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
    use crate::{BusEvent, SlotId, TrackId};

    #[derive(Clone, Debug, PartialEq, Eq, crate::Event)]
    enum TestEvent {
        EndOfStream,
        ReadProgress { position: u64, total: Option<u64> },
        Error { error: String },
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, crate::Event)]
    struct Early(u8);

    #[derive(Clone, Copy, Debug, PartialEq, Eq, crate::Event)]
    struct Late(u8);

    /// `Late` is declared first so that declaration order and publication
    /// order can disagree.
    #[derive(Clone, Debug, crate::EventSet)]
    enum Pair {
        Late(Late),
        Early(Early),
    }

    #[kithara::test]
    fn publish_without_subscribers_does_not_panic() {
        let bus = EventBus::new(16);
        bus.publish(TestEvent::EndOfStream);
    }

    #[kithara::test(tokio)]
    #[case(TestEvent::ReadProgress { position: 42, total: None })]
    #[case(TestEvent::EndOfStream)]
    async fn publish_and_subscribe(#[case] expected: TestEvent) {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe::<TestEvent>();
        bus.publish(expected.clone());
        let event = rx.recv().await.unwrap();
        assert_eq!(event.event, expected);
    }

    #[kithara::test(tokio)]
    #[case(TestEvent::EndOfStream)]
    #[case(TestEvent::Error {
        error: String::from("network".to_string()),
    })]
    async fn multiple_subscribers_each_receive(#[case] expected: TestEvent) {
        let bus = EventBus::new(16);
        let mut rx1 = bus.subscribe::<TestEvent>();
        let mut rx2 = bus.subscribe::<TestEvent>();
        bus.publish(expected.clone());
        assert_eq!(rx1.recv().await.unwrap().event, expected);
        assert_eq!(rx2.recv().await.unwrap().event, expected);
    }

    #[kithara::test(tokio)]
    async fn lagged_subscriber_gets_error() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe::<TestEvent>();
        for i in 0..10 {
            bus.publish(TestEvent::ReadProgress {
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

    #[kithara::test]
    fn clone_shares_channel() {
        let bus1 = EventBus::new(16);
        let bus2 = bus1.clone();
        let mut rx = bus1.subscribe::<TestEvent>();
        bus2.publish(TestEvent::EndOfStream);
        assert!(rx.try_recv().is_ok());
    }

    #[kithara::test(tokio)]
    #[case::root_sees_child(true)]
    #[case::child_sees_own(false)]
    async fn child_publish_is_visible_to_subscriber(#[case] subscribe_from_root: bool) {
        let root = EventBus::new(16);
        let child = root.scoped();
        let mut rx = if subscribe_from_root {
            root.subscribe::<TestEvent>()
        } else {
            child.subscribe::<TestEvent>()
        };

        child.publish(TestEvent::EndOfStream);
        let event = rx.recv().await.unwrap();
        assert_eq!(event.event, TestEvent::EndOfStream);
    }

    #[kithara::test(tokio)]
    async fn child_does_not_see_sibling_events() {
        let root = EventBus::new(16);
        let child_a = root.scoped();
        let child_b = root.scoped();
        let mut rx_a = child_a.subscribe::<TestEvent>();

        child_b.publish(TestEvent::EndOfStream);

        assert!(rx_a.try_recv().is_err());
    }

    #[kithara::test(tokio)]
    async fn root_sees_grandchild_events() {
        let root = EventBus::new(16);
        let child = root.scoped();
        let grandchild = child.scoped();
        let mut rx = root.subscribe::<TestEvent>();

        grandchild.publish(TestEvent::EndOfStream);
        let event = rx.recv().await.unwrap();
        assert_eq!(event.event, TestEvent::EndOfStream);
    }

    #[kithara::test(tokio)]
    async fn parent_sees_child_but_not_sibling() {
        let root = EventBus::new(16);
        let child_a = root.scoped();
        let child_b = root.scoped();
        let mut rx_root = root.subscribe::<TestEvent>();
        let mut rx_a = child_a.subscribe::<TestEvent>();

        child_a.publish(TestEvent::EndOfStream);
        child_b.publish(TestEvent::ReadProgress {
            position: 99,
            total: None,
        });

        let e1 = rx_root.recv().await.unwrap();
        let e2 = rx_root.recv().await.unwrap();
        assert_eq!(e1.event, TestEvent::EndOfStream);
        assert_eq!(
            e2.event,
            TestEvent::ReadProgress {
                position: 99,
                total: None,
            },
        );

        let ea = rx_a.recv().await.unwrap();
        assert_eq!(ea.event, TestEvent::EndOfStream);
        assert!(rx_a.try_recv().is_err());
    }

    /// The bus carries one channel per event type, so a receiver spanning
    /// several of them has no cross-topic order to report: it hands back
    /// whichever member it polls first. A caller that wants to know what was
    /// published first has to subscribe to that one topic.
    #[kithara::test(tokio)]
    async fn a_receiver_over_several_topics_does_not_preserve_publication_order() {
        let bus = EventBus::new(16);
        let mut pair = bus.subscribe::<Pair>();
        let mut early = bus.subscribe::<Early>();

        bus.publish(Early(1));
        bus.publish(Late(2));

        assert!(matches!(
            pair.try_recv().map(|envelope| envelope.event),
            Ok(Pair::Late(Late(2)))
        ));
        assert_eq!(early.try_recv().unwrap().event, Early(1));
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

    #[kithara::test(tokio)]
    async fn envelope_meta_stamps_scope_seq_and_time() {
        let root = EventBus::new(16);
        let child_a = root.scoped_labeled(ScopeLabel {
            track: Some(TrackId(7)),
            ..ScopeLabel::default()
        });
        let child_b = root.scoped();
        let mut rx = root.subscribe::<TestEvent>();

        child_a.publish(TestEvent::EndOfStream);
        child_b.publish(TestEvent::EndOfStream);

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

    #[kithara::test(tokio)]
    async fn overflow_event_publishes_exact_drop_count() {
        let bus = EventBus::new(16);
        let deferred = crate::DeferredBus::<TestEvent>::new(bus.clone(), 2);
        let mut rx = bus.subscribe::<BusEvent>();

        deferred.enqueue(TestEvent::EndOfStream);
        deferred.enqueue(TestEvent::EndOfStream);
        deferred.enqueue(TestEvent::EndOfStream);
        deferred.enqueue(TestEvent::EndOfStream);
        deferred.flush();

        let overflow = rx.recv().await.unwrap();
        match overflow.event {
            BusEvent::Overflow { scope, dropped } => {
                assert_eq!(scope, bus.id());
                assert_eq!(dropped, 2);
            }
        }
    }
}

#[cfg(test)]
mod typed_tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Ping(u64);

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Pong(u64);

    impl Event for Ping {}
    impl Event for Pong {}

    #[derive(Clone, Debug, PartialEq, Eq, crate::EventSet)]
    enum Probe {
        Ping(Ping),
        Pong(Pong),
    }
    #[kithara::test]
    async fn a_publish_reaches_a_typed_subscriber() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe::<Ping>();

        bus.publish(Ping(1));

        assert_eq!(rx.try_recv().expect("the ping arrives").event, Ping(1));
    }

    #[kithara::test]
    async fn a_publish_of_an_unsubscribed_type_is_dropped() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe::<Ping>();

        bus.publish(Pong(1));
        bus.publish(Ping(2));

        assert_eq!(rx.try_recv().expect("only the ping arrives").event, Ping(2));
    }

    #[kithara::test]
    async fn a_child_publish_reaches_the_root_subscriber() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe::<Ping>();
        let child = bus.scoped();

        child.publish(Ping(3));

        let envelope = rx.try_recv().expect("the child event reaches the root");
        assert_eq!(envelope.event, Ping(3));
        assert_eq!(envelope.meta.origin, child.id());
    }

    #[kithara::test]
    async fn a_root_publish_does_not_reach_a_child_subscriber() {
        let bus = EventBus::default();
        let child = bus.scoped();
        let mut rx = child.subscribe::<Ping>();

        bus.publish(Ping(4));

        assert!(rx.try_recv().is_err(), "the root event stays at the root");
    }

    #[kithara::test]
    async fn a_set_receiver_merges_both_member_channels() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe::<Probe>();

        bus.publish(Ping(5));
        bus.publish(Pong(6));

        assert_eq!(
            rx.recv().await.expect("the ping arrives").event,
            Probe::Ping(Ping(5))
        );
        assert_eq!(
            rx.recv().await.expect("the pong arrives").event,
            Probe::Pong(Pong(6))
        );
    }
}
