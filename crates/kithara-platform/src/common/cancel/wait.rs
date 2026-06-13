use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use super::node::{Node, Slot};

/// Future returned by [`CancelToken::cancelled`](super::CancelToken::cancelled).
/// Resolves once the token's subtree is cancelled. `Unpin`; cancel-safe (drop
/// unregisters its slot).
pub struct Cancelled<'a> {
    node: &'a Arc<Node>,
    slot: Option<u64>,
    done: bool,
}

impl<'a> Cancelled<'a> {
    pub(super) fn new(node: &'a Arc<Node>) -> Self {
        Self {
            node,
            slot: None,
            done: false,
        }
    }
}

impl Future for Cancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let me = self.get_mut();
        poll_cancelled(
            &mut Fields {
                node: me.node,
                slot: &mut me.slot,
                done: &mut me.done,
            },
            cx,
        )
    }
}

impl Drop for Cancelled<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.slot.take() {
            self.node.unregister(id);
        }
    }
}

impl fmt::Debug for Cancelled<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cancelled").finish_non_exhaustive()
    }
}

/// `'static` flavour of [`Cancelled`]. Holds an owned `Arc<Node>` so it can be
/// spawned. Same slot mechanics.
pub struct CancelledOwned {
    node: Arc<Node>,
    slot: Option<u64>,
    done: bool,
}

impl CancelledOwned {
    pub(super) fn new(node: Arc<Node>) -> Self {
        Self {
            node,
            slot: None,
            done: false,
        }
    }
}

impl Future for CancelledOwned {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let me = self.get_mut();
        let node = Arc::clone(&me.node);
        poll_cancelled(
            &mut Fields {
                node: &node,
                slot: &mut me.slot,
                done: &mut me.done,
            },
            cx,
        )
    }
}

impl Drop for CancelledOwned {
    fn drop(&mut self) {
        if let Some(id) = self.slot.take() {
            self.node.unregister(id);
        }
    }
}

impl fmt::Debug for CancelledOwned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelledOwned").finish_non_exhaustive()
    }
}

struct Fields<'a> {
    node: &'a Arc<Node>,
    slot: &'a mut Option<u64>,
    done: &'a mut bool,
}

fn poll_cancelled(f: &mut Fields<'_>, cx: &mut Context<'_>) -> Poll<()> {
    if *f.done {
        return Poll::Ready(());
    }
    if f.node.is_cancelled() {
        if let Some(id) = f.slot.take() {
            f.node.unregister(id);
        }
        *f.done = true;
        return Poll::Ready(());
    }
    if let Some(id) = *f.slot {
        f.node.refresh_task(id, cx.waker());
        return Poll::Pending;
    }
    // Register, then handle the race: cancel may have fired between the
    // is_cancelled() above and the registration. register() returns None if the
    // node already fired (born/late) — resolve at once.
    if let Some(id) = f.node.register(Slot::Task(cx.waker().clone())) {
        *f.slot = Some(id);
        Poll::Pending
    } else {
        *f.done = true;
        Poll::Ready(())
    }
}
