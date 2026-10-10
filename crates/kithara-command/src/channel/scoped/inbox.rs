use futures::task::AtomicWaker;
use kithara_platform::sync::Arc;
use ringbuf::{HeapCons, HeapProd, traits::Consumer};

use super::{Item, ScopeDocket, ScopeId, ScopeReply};
use crate::{
    LevelInbox, Outcome, Protocol, Receipt, Rejection,
    channel::{docket::Docket, gate::Gate, sink::Sink},
};

/// The shared command-ring consumer and the preallocated executor dockets.
pub struct ScopedInbox<R: Protocol, M: Protocol> {
    pub(super) pending: HeapCons<Item<R, M>>,
    pub(super) root_answers: HeapProd<Receipt<R>>,
    pub(super) scope_answers: HeapProd<ScopeReply<M>>,
    pub(super) root: Docket<R>,
    pub(super) scopes: Vec<ScopeDocket<M>>,
    pub(super) gate: Arc<Gate>,
    pub(super) answered: Arc<AtomicWaker>,
}

impl<R: Protocol, M: Protocol> ScopedInbox<R, M> {
    /// Routes committed batches into their level without judging their basis or moment.
    pub fn drain(&mut self) {
        while let Some(item) = self.pending.try_pop() {
            match item {
                Item::Root(sent) => self.root.insert(sent),
                Item::Scope { id, sent } => {
                    let scope = &mut self.scopes[usize::from(id.index)];
                    if scope.lifecycle.generation == id.generation {
                        scope.docket.insert(sent);
                    } else {
                        Sink::Scope {
                            answers: &mut self.scope_answers,
                            id,
                        }
                        .reply(
                            Receipt {
                                seq: sent.seq,
                                batch: sent.batch,
                                outcome: Outcome::Rejected(Rejection::Unanswered),
                            },
                            &self.answered,
                        );
                        debug_assert_eq!(
                            scope.lifecycle.generation, id.generation,
                            "a batch keeps its scope generation"
                        );
                    }
                }
                Item::Close(id) => {
                    let scope = &mut self.scopes[usize::from(id.index)];
                    if scope.lifecycle.generation == id.generation {
                        scope.lifecycle.closing = true;
                    }
                }
            }
        }
    }

    /// Borrows the root's one-level executor.
    pub fn root(&mut self) -> LevelInbox<'_, R> {
        LevelInbox {
            docket: &mut self.root,
            sink: Sink::Plain(&mut self.root_answers),
            answered: &self.answered,
            lifecycle: None,
        }
    }

    /// Borrows a scope only while its generation matches this identity.
    pub fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, M>> {
        let scope = self.scopes.get_mut(usize::from(id.index))?;
        if scope.lifecycle.generation != id.generation {
            return None;
        }
        Some(scope.level(id.index, &mut self.scope_answers, &self.answered))
    }

    /// Drains and refuses every At on every level, preserving Next, Deferred and parked batches.
    pub fn refuse_timed(&mut self, root: R::Refusal, scope: M::Refusal)
    where
        R::Refusal: Clone,
        M::Refusal: Clone,
    {
        self.drain();
        self.root().refuse_timed(root);
        for (index, docket) in (0..).zip(&mut self.scopes) {
            docket
                .level(index, &mut self.scope_answers, &self.answered)
                .refuse_timed(scope.clone());
        }
    }

    /// With no executor turn in flight, drains and retires every closing scope.
    pub fn retire_closing(&mut self) {
        self.drain();
        for (index, scope) in (0..).zip(&mut self.scopes) {
            if scope.lifecycle.closing {
                scope
                    .level(index, &mut self.scope_answers, &self.answered)
                    .retire();
            }
        }
    }
}

impl<P: Protocol> ScopeDocket<P> {
    fn level<'inbox>(
        &'inbox mut self,
        index: u16,
        answers: &'inbox mut HeapProd<ScopeReply<P>>,
        answered: &'inbox AtomicWaker,
    ) -> LevelInbox<'inbox, P> {
        LevelInbox {
            docket: &mut self.docket,
            sink: Sink::Scope {
                answers,
                id: ScopeId {
                    index,
                    generation: self.lifecycle.generation,
                },
            },
            answered,
            lifecycle: Some(&mut self.lifecycle),
        }
    }
}

impl<R: Protocol, M: Protocol> Drop for ScopedInbox<R, M> {
    fn drop(&mut self) {
        self.gate.close();
        self.drain();
        self.root().unanswered();
        for (index, scope) in (0..).zip(&mut self.scopes) {
            scope
                .level(index, &mut self.scope_answers, &self.answered)
                .unanswered();
        }
    }
}
