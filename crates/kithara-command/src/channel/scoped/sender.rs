use std::task::Waker;

use futures::task::AtomicWaker;
use kithara_platform::sync::Arc;
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Consumer, Observer, Producer},
};

use super::{Closed, Item, OpenError, ScopeId, ScopeReply, ScopedReceipt, Slot, StaleScope, State};
use crate::{
    Batch, Port, Protocol, SendError, Seq, When,
    channel::{book::Book, gate::Gate, sender::Sent},
};

type Commands<R, M> = HeapProd<Item<R, M>>;

/// The owner-thread producer, staging every level until one explicit publication.
pub struct ScopedSender<R: Protocol, M: Protocol> {
    pub(super) commands: Commands<R, M>,
    pub(super) staged: Vec<Item<R, M>>,
    pub(super) root_receipts: HeapCons<crate::Receipt<R>>,
    pub(super) scope_receipts: HeapCons<ScopeReply<M>>,
    pub(super) root: Book<R>,
    pub(super) slots: Vec<Slot<M>>,
    pub(super) scope_targets: usize,
    pub(super) next: Seq,
    pub(super) gate: Arc<Gate>,
    pub(super) answered: Arc<AtomicWaker>,
    pub(super) holder: Option<Waker>,
}

/// A temporary command port into one scope generation.
pub struct ScopeSender<'sender, R: Protocol, M: Protocol> {
    sender: &'sender mut ScopedSender<R, M>,
    index: u16,
}

impl<R: Protocol, M: Protocol> ScopedSender<R, M> {
    /// Opens a free slot, checking its target count before looking for room.
    ///
    /// # Errors
    /// Returns [`OpenError::Targets`] before [`OpenError::Exhausted`].
    pub fn open(&mut self, targets: usize) -> Result<ScopeId, OpenError> {
        if targets > self.scope_targets {
            return Err(OpenError::Targets {
                targets,
                limit: self.scope_targets,
            });
        }
        let (index, slot) = (0..)
            .zip(&mut self.slots)
            .find(|(_, slot)| slot.state == State::Free)
            .ok_or(OpenError::Exhausted)?;
        slot.book.reset(targets);
        slot.state = State::Open;
        Ok(ScopeId {
            index,
            generation: slot.generation,
        })
    }

    /// Borrows an open or closing scope of this generation.
    pub fn scope(&mut self, id: ScopeId) -> Option<ScopeSender<'_, R, M>> {
        let slot = self.slots.get(usize::from(id.index))?;
        if slot.generation != id.generation || slot.state == State::Free {
            return None;
        }
        Some(ScopeSender {
            sender: self,
            index: id.index,
        })
    }

    /// Queues Close on the slot's reserved credit; normal batches cannot fill it.
    ///
    /// # Errors
    /// Returns [`StaleScope`] unless this generation is open.
    pub fn close(&mut self, id: ScopeId) -> Result<(), StaleScope> {
        let slot = self
            .slots
            .get_mut(usize::from(id.index))
            .ok_or(StaleScope)?;
        if slot.generation != id.generation || slot.state != State::Open {
            return Err(StaleScope);
        }
        debug_assert!(
            self.staged.len() < self.commands.vacant_len(),
            "each scope reserves one Close credit"
        );
        self.staged.push(Item::Close(id));
        slot.state = State::Closing;
        Ok(())
    }

    /// Publishes everything staged in this pass with one ring-index store.
    ///
    /// # Errors
    /// On [`Closed`], discards unpublished batches on the owner thread.
    pub fn publish(&mut self) -> Result<(), Closed> {
        if self.gate.is_closed() {
            self.staged.clear();
            return Err(Closed);
        }
        if self.staged.is_empty() {
            return Ok(());
        }
        if !self.gate.enter() {
            self.staged.clear();
            return Err(Closed);
        }
        let staged = self.staged.len();
        let published = self.commands.push_iter(self.staged.drain(..));
        self.gate.leave();
        debug_assert_eq!(published, staged, "admitted pass fits in the command ring");
        Ok(())
    }

    /// Reads and settles one root receipt without consuming scope replies.
    pub fn root_receipt(&mut self) -> Option<crate::Receipt<R>> {
        if let Some(holder) = &self.holder {
            self.answered.register(holder);
        }
        let receipt = self.root_receipts.try_pop()?;
        self.root.settle(&receipt);
        Some(receipt)
    }

    /// Reads one receipt, root first, and settles the corresponding level's credits and basis.
    pub fn receipt(&mut self) -> Option<ScopedReceipt<R, M>> {
        if let Some(receipt) = self.root_receipt() {
            return Some(ScopedReceipt::Root(receipt));
        }
        let reply = self.scope_receipts.try_pop()?;
        match reply {
            ScopeReply::Receipt { id, receipt } => {
                let slot = &mut self.slots[usize::from(id.index)];
                debug_assert_eq!(
                    slot.generation, id.generation,
                    "a reply keeps its batch generation"
                );
                if slot.generation == id.generation {
                    slot.book.settle(&receipt);
                }
                Some(ScopedReceipt::Scope(id, receipt))
            }
            ScopeReply::Closed(id) => {
                let slot = &mut self.slots[usize::from(id.index)];
                debug_assert_eq!(
                    slot.generation, id.generation,
                    "Closed follows every batch of its generation"
                );
                if slot.generation == id.generation {
                    slot.generation = id.generation.wrapping_add(1);
                    slot.state = State::Free;
                }
                Some(ScopedReceipt::Closed(id))
            }
        }
    }

    /// Receipts wake this owner until it releases the registration.
    pub fn hold(&mut self, waker: Waker) {
        self.answered.register(&waker);
        self.holder = Some(waker);
    }

    /// Stops waking the owner; queued receipts remain readable.
    pub fn release(&mut self) {
        self.holder = None;
        self.answered.register(Waker::noop());
    }
}

impl<R: Protocol, M: Protocol> ScopeSender<'_, R, M> {
    /// This port's slot and generation.
    #[must_use]
    pub fn id(&self) -> ScopeId {
        ScopeId {
            index: self.index,
            generation: self.sender.slots[usize::from(self.index)].generation,
        }
    }
}

impl<R: Protocol, M: Protocol> Port<R> for ScopedSender<R, M> {
    fn send(&mut self, when: When<R::Clock>, batch: Batch<R>) -> Result<Seq, SendError<R>> {
        if self.gate.is_closed() {
            return Err(SendError::Closed(batch));
        }
        push(
            &self.commands,
            &mut self.staged,
            &mut self.next,
            &mut self.root,
            when,
            batch,
            Item::Root,
        )
    }

    delegate::delegate! {
        to self.root {
            fn basis(&self, target: R::Target, when: When<R::Clock>) -> Option<Seq>;
            fn available(&self) -> usize;
        }
    }
}

impl<R: Protocol, M: Protocol> Port<M> for ScopeSender<'_, R, M> {
    fn send(&mut self, when: When<M::Clock>, batch: Batch<M>) -> Result<Seq, SendError<M>> {
        let id = self.id();
        let slot = &mut self.sender.slots[usize::from(self.index)];
        if slot.state != State::Open || self.sender.gate.is_closed() {
            return Err(SendError::Closed(batch));
        }
        push(
            &self.sender.commands,
            &mut self.sender.staged,
            &mut self.sender.next,
            &mut slot.book,
            when,
            batch,
            |sent| Item::Scope { id, sent },
        )
    }

    delegate::delegate! {
        to self.sender.slots[usize::from(self.index)]
            .book {
            fn basis(&self, target: M::Target, when: When<M::Clock>) -> Option<Seq>;
            fn available(&self) -> usize;
        }
    }
}

fn push<R: Protocol, M: Protocol, P: Protocol>(
    commands: &Commands<R, M>,
    staged: &mut Vec<Item<R, M>>,
    next: &mut Seq,
    book: &mut Book<P>,
    when: When<P::Clock>,
    batch: Batch<P>,
    wrap: impl FnOnce(Sent<P>) -> Item<R, M>,
) -> Result<Seq, SendError<P>> {
    if !book.admits(&batch) {
        return Err(SendError::Target(batch));
    }
    if book.available() == 0 || commands.vacant_len() <= staged.len() {
        return Err(SendError::Full(batch));
    }
    let seq = *next;
    let basis = batch.basis.clone();
    staged.push(wrap(Sent { batch, seq, when }));
    book.spend(when, seq, &basis);
    *next = seq.next();
    Ok(seq)
}

impl<R: Protocol, M: Protocol> Drop for ScopedSender<R, M> {
    fn drop(&mut self) {
        self.staged.clear();
    }
}
