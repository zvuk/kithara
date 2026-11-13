#![forbid(unsafe_code)]

use core::any::Any;

use kithara_platform::{sync::OnceLock, tokio::sync::broadcast};

use crate::{Envelope, Event};

pub(crate) struct Topic<E: Event> {
    tx: broadcast::Sender<Envelope<E>>,
}

impl<E: Event> Topic<E> {
    fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub(crate) fn send(&self, envelope: Envelope<E>) {
        self.tx.send(envelope).ok();
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Envelope<E>> {
        self.tx.subscribe()
    }
}

struct Node {
    topic: Box<dyn Any + Send + Sync>,
    next: OnceLock<Box<Self>>,
}

/// The channels one bus scope owns, one per event type, created on first use.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ScopeTopics {
    head: OnceLock<Box<Node>>,
    #[field(get, vis = "pub(crate)")]
    capacity: usize,
}

impl ScopeTopics {
    pub(crate) const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            head: OnceLock::new(),
        }
    }

    pub(crate) fn find<E: Event>(&self) -> Option<&Topic<E>> {
        let mut slot = &self.head;
        while let Some(node) = slot.get() {
            if let Some(topic) = node.topic.downcast_ref::<Topic<E>>() {
                return Some(topic);
            }
            slot = &node.next;
        }
        None
    }

    pub(crate) fn find_or_insert<E: Event>(&self) -> &Topic<E> {
        let mut slot = &self.head;
        loop {
            let node = slot.get_or_init(|| {
                Box::new(Node {
                    topic: Box::new(Topic::<E>::new(self.capacity)),
                    next: OnceLock::new(),
                })
            });
            if let Some(topic) = node.topic.downcast_ref::<Topic<E>>() {
                return topic;
            }
            slot = &node.next;
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[derive(Clone, Debug)]
    struct A;
    #[derive(Clone, Debug)]
    struct B;

    impl Event for A {}
    impl Event for B {}

    #[kithara::test]
    async fn find_returns_nothing_before_the_first_insert() {
        let topics = ScopeTopics::new(4);

        assert!(topics.find::<A>().is_none());
    }

    #[kithara::test]
    async fn find_or_insert_is_idempotent_per_type() {
        let topics = ScopeTopics::new(4);

        let first = topics.find_or_insert::<A>() as *const Topic<A>;
        let second = topics.find_or_insert::<A>() as *const Topic<A>;

        assert_eq!(first, second, "the same type resolves to the same channel");
    }

    #[kithara::test]
    async fn two_types_get_two_channels() {
        let topics = ScopeTopics::new(4);

        topics.find_or_insert::<A>();
        topics.find_or_insert::<B>();

        assert!(topics.find::<A>().is_some());
        assert!(topics.find::<B>().is_some());
    }

    #[kithara::test(miri)]
    async fn a_concurrent_insert_of_the_same_type_yields_one_channel() {
        let topics = std::sync::Arc::new(ScopeTopics::new(4));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let topics = std::sync::Arc::clone(&topics);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    topics.find_or_insert::<A>() as *const Topic<A> as usize
                })
            })
            .collect();

        let addresses: Vec<usize> = handles
            .into_iter()
            .map(|handle| handle.join().expect("the thread finishes"))
            .collect();

        assert!(
            addresses.windows(2).all(|pair| pair[0] == pair[1]),
            "every racing insert resolves to the same channel"
        );
    }
}
