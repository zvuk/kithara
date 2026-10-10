use super::{ledger::Ledger, sender::Sent};
use crate::protocol::{Protocol, When};

/// Batches waiting for their moment, the next one to run last.
///
/// Storage is reserved once for the channel's capacity; the sender's credits
/// keep the batches within it, so inserting and popping never reallocate.
pub(super) struct Schedule<P: Protocol> {
    items: Vec<Sent<P>>,
}

impl<P: Protocol> Schedule<P> {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
        }
    }

    pub(super) fn insert(&mut self, sent: Sent<P>) {
        debug_assert!(
            self.items.len() < self.items.capacity(),
            "credits bound the batches in flight"
        );
        let key = sent.key();
        let index = self.items.partition_point(|item| item.key() > key);
        self.items.insert(index, sent);
    }

    /// Takes the earliest batch waiting for a moment of the clock.
    pub(super) fn take_timed(&mut self) -> Option<Sent<P>> {
        let timed = self
            .items
            .partition_point(|sent| matches!(sent.when, When::At(_)));
        timed.checked_sub(1).map(|index| self.items.remove(index))
    }

    pub(super) fn take_outdated(&mut self, ledger: &Ledger) -> Option<Sent<P>> {
        let index = self
            .items
            .iter()
            .rposition(|sent| ledger.outdates(&sent.batch.basis))?;
        Some(self.items.remove(index))
    }

    delegate::delegate! {
        to self.items {
            #[expr($.map(|sent| sent.when))]
            #[call(last)]
            pub(super) fn peek(&self) -> Option<When<P::Clock>>;
            pub(super) fn pop(&mut self) -> Option<Sent<P>>;
        }
    }
}
