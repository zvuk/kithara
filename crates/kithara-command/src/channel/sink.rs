use futures::task::AtomicWaker;
use ringbuf::{HeapProd, traits::Producer};

use super::scoped::{ScopeId, ScopeReply};
use crate::{Protocol, Receipt};

pub(super) enum Sink<'inbox, P: Protocol> {
    Plain(&'inbox mut HeapProd<Receipt<P>>),
    Scope {
        answers: &'inbox mut HeapProd<ScopeReply<P>>,
        id: ScopeId,
    },
}

impl<P: Protocol> Sink<'_, P> {
    pub(super) fn reborrow(&mut self) -> Sink<'_, P> {
        match self {
            Self::Plain(answers) => Sink::Plain(answers),
            Self::Scope { answers, id } => Sink::Scope { answers, id: *id },
        }
    }

    pub(super) fn reply(&mut self, receipt: Receipt<P>, answered: &AtomicWaker) {
        let pushed = match self {
            Self::Plain(answers) => answers.try_push(receipt).is_ok(),
            Self::Scope { answers, id } => answers
                .try_push(ScopeReply::Receipt { id: *id, receipt })
                .is_ok(),
        };
        debug_assert!(pushed, "credits bound receipts");
        answered.wake();
    }

    pub(super) fn closed(&mut self, answered: &AtomicWaker) {
        if let Self::Scope { answers, id } = self {
            let pushed = answers.try_push(ScopeReply::Closed(*id));
            debug_assert!(pushed.is_ok(), "each scope reserves one Closed credit");
            answered.wake();
        }
    }
}
