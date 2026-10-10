use futures::task::AtomicWaker;
use kithara_platform::sync::Arc;
use ringbuf::{HeapRb, traits::Split};

use super::{
    Item, Lifecycle, ScopeDocket, ScopeReply, ScopedConfig, ScopedInbox, ScopedSender, Slot, State,
};
use crate::{
    Protocol, Receipt, Seq,
    channel::{book::Book, docket::Docket, gate::Gate},
};

/// Builds a root and its scope slots with independent credits and one publication ring.
#[must_use]
pub fn scoped_channel<R: Protocol, M: Protocol>(
    config: ScopedConfig,
) -> (ScopedSender<R, M>, ScopedInbox<R, M>) {
    let scopes = usize::from(config.scopes.get());
    let scope_capacity = scopes * (config.scope.capacity.get() + 1);
    let command_capacity = config.root.capacity.get() + scope_capacity;
    let (commands, pending) = HeapRb::<Item<R, M>>::new(command_capacity).split();
    let (root_answers, root_receipts) =
        HeapRb::<Receipt<R>>::new(config.root.capacity.get()).split();
    let (scope_answers, scope_receipts) = HeapRb::<ScopeReply<M>>::new(scope_capacity).split();
    let gate = Arc::new(Gate::default());
    let answered = Arc::new(AtomicWaker::new());
    let sender = ScopedSender {
        commands,
        staged: Vec::with_capacity(command_capacity),
        root_receipts,
        scope_receipts,
        root: Book::new(config.root.capacity.get(), config.root.targets),
        slots: (0..config.scopes.get())
            .map(|_| Slot {
                state: State::Free,
                generation: 0,
                book: Book::new(config.scope.capacity.get(), config.scope.targets),
            })
            .collect(),
        scope_targets: config.scope.targets,
        next: Seq::FIRST,
        gate: Arc::clone(&gate),
        answered: Arc::clone(&answered),
        holder: None,
    };
    let inbox = ScopedInbox {
        pending,
        root_answers,
        scope_answers,
        root: Docket::new(config.root),
        scopes: (0..config.scopes.get())
            .map(|_| ScopeDocket {
                docket: Docket::new(config.scope),
                lifecycle: Lifecycle {
                    generation: 0,
                    closing: false,
                },
            })
            .collect(),
        gate,
        answered,
    };
    (sender, inbox)
}
