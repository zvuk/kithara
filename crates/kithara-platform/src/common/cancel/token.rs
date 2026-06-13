use std::{fmt, sync::Arc};

use super::{
    node::{Node, Slot},
    wait::Cancelled,
};

/// A handle into a cancel subtree.
///
/// [`Clone`] yields the **same** identity (same node), like the legacy token. A
/// `cancel()` cancels this token's own subtree (epoch/fetch cancels are
/// legitimate); ancestors and siblings are unaffected. Two constructors mint a
/// fresh subtree root instead of deriving from a parent —
/// [`root`](CancelToken::root), the owning master a subsystem holds and cancels
/// on teardown, and [`never`](CancelToken::never), a sentinel that is never
/// cancelled. Both are covered by the `cancel_root_sites` guard.
#[derive(Clone)]
pub struct CancelToken {
    node: Arc<Node>,
}

impl CancelToken {
    /// Mint a fresh root of a new cancel subtree — the owning master a subsystem
    /// holds and cancels on teardown. `Drop` is passive: dropping the root does
    /// **not** cancel its subtree; teardown is an explicit `cancel()`. Minted only
    /// at owner sites (enforced by the `cancel_root_sites` guard). For a token that
    /// is structurally required but never cancelled, use
    /// [`never`](CancelToken::never) instead.
    #[must_use]
    pub fn root() -> Self {
        Self { node: Node::root() }
    }

    /// A token that is never cancelled — a placeholder where a token is required
    /// but no cancellation source exists. The sentinel sibling of
    /// [`root`](CancelToken::root); both mint a fresh subtree root.
    #[must_use]
    pub fn never() -> Self {
        Self { node: Node::root() }
    }

    /// Derive a child token rooted at this token's subtree.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            node: Node::child(&self.node),
        }
    }

    /// Cancel this token's subtree.
    pub fn cancel(&self) {
        self.node.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.node.is_cancelled()
    }

    /// Future that resolves once this token's subtree is cancelled. Cancel-safe
    /// in `tokio::select!`: dropping it unregisters its slot.
    #[must_use]
    pub fn cancelled(&self) -> Cancelled<'_> {
        Cancelled::new(&self.node)
    }

    /// Register a synchronous waker fired when this token is cancelled — the sync
    /// counterpart to [`cancelled`](CancelToken::cancelled) for a thread parked
    /// on a (flash-aware) `Condvar`/`Notify`. Registered on THIS node only; an
    /// ancestor `cancel()` reaches it through the down-propagation drain.
    ///
    /// The waker runs on the cancelling thread: keep it cheap, non-blocking, and
    /// idempotent. If the token is already cancelled it fires immediately. The
    /// returned guard unregisters the waker on drop — hold it for the wait's
    /// lifetime.
    pub fn on_cancel<F>(&self, waker: F) -> CancelWakerGuard
    where
        F: Fn() + Send + Sync + 'static,
    {
        let id = self.node.register(Slot::Sync(Arc::new(waker)));
        CancelWakerGuard {
            node: id.map(|id| (Arc::clone(&self.node), id)),
        }
    }
}

impl fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Drop guard returned by [`CancelToken::on_cancel`]. Unregisters the waker on
/// drop; holding it keeps the cancel-wake live.
#[must_use = "dropping the guard immediately unregisters the cancel waker"]
pub struct CancelWakerGuard {
    node: Option<(Arc<Node>, u64)>,
}

impl Drop for CancelWakerGuard {
    fn drop(&mut self) {
        if let Some((node, id)) = &self.node {
            node.unregister(*id);
        }
    }
}

impl fmt::Debug for CancelWakerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelWakerGuard")
            .field("registered", &self.node.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use kithara_test_utils::kithara;
    use tokio::{spawn, task, time as tokio_time};

    use super::CancelToken;

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fresh_token_not_cancelled() {
        let c = CancelToken::root();
        assert!(!c.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cancel_sets_lock_free_flag() {
        let c = CancelToken::root();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_cancel_does_not_cancel_parent() {
        // WHY: cancelling a child sets only the child's subtree, never the
        // parent's.
        let parent = CancelToken::root();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(
            !parent.is_cancelled(),
            "child cancel must not cancel the parent"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_cancel_does_not_cancel_sibling() {
        let parent = CancelToken::root();
        let a = parent.child();
        let b = parent.child();
        a.cancel();
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled(), "sibling cancel must stay independent");
        assert!(!parent.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn clone_shares_cancel_identity() {
        // WHY: Clone is the SAME token (same node), so cancelling a clone is
        // observed by the original — distinct from child().
        let token = CancelToken::never();
        let twin = token.clone();
        twin.cancel();
        assert!(
            token.is_cancelled(),
            "clone shares identity with the original"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_observes_own_cancel() {
        let parent = CancelToken::root();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
    }

    /// A worker token derived via `child()` must observe a master `cancel()`
    /// through its lock-free read, even though nobody calls the worker token's
    /// own `cancel()`.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_observes_parent_cancel_lock_free() {
        let master = CancelToken::root();
        let worker = master.child();
        assert!(!worker.is_cancelled());

        std::thread::scope(|s| {
            s.spawn(|| master.cancel());
        });

        assert!(
            worker.is_cancelled(),
            "worker child must observe master cancel via propagate-down"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn grandchild_observes_master_cancel_lock_free() {
        let master = CancelToken::root();
        let mid = master.child();
        let leaf = mid.child();

        master.cancel();
        assert!(mid.is_cancelled());
        assert!(leaf.is_cancelled());
    }

    /// PARENT-LIVENESS PIN: drop every clone of an intermediate token, then
    /// cancel the root — the grandchild (held alive) must still observe it. The
    /// legacy grandchild test keeps `mid` alive and would not catch a missing
    /// `parent` liveness field; this one drops it.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn root_cancel_reaches_grandchild_after_intermediate_dropped() {
        let master = CancelToken::root();
        let leaf = {
            let mid = master.child();
            mid.child()
            // `mid` dropped here — its node must stay alive via `leaf.parent`.
        };
        assert!(!leaf.is_cancelled());
        master.cancel();
        assert!(
            leaf.is_cancelled(),
            "root cancel must reach the grandchild after the intermediate token dropped"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn repeat_cancel_is_idempotent() {
        let c = CancelToken::root();
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn late_child_born_cancelled() {
        let master = CancelToken::root();
        master.cancel();
        let child = master.child();
        assert!(
            child.is_cancelled(),
            "a child born after the parent cancelled must be born cancelled"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_churn_then_cancel_reaches_survivors() {
        // End-to-end: after heavy per-fetch/epoch child churn, a master cancel
        // still reaches the live children. The dead-Weak sweep BOUND itself is
        // pinned at the node layer (`node::tests::child_churn_sweeps_dead_weaks`),
        // which is the only place the private `children` len is observable.
        let master = CancelToken::root();
        for _ in 0..1000 {
            let _ = master.child();
        }
        let survivor = master.child();
        let _second = master.child();
        master.cancel();
        assert!(survivor.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn async_cancelled_resolves_on_self_cancel() {
        // Node mechanism: cancelled() resolves when this node cancels; the flag
        // is visible after it resolves.
        let c = CancelToken::never();
        let c2 = c.clone();
        let handle = spawn(async move {
            c2.cancelled().await;
            c2.is_cancelled()
        });
        task::yield_now().await;

        c.cancel();

        let flag = tokio_time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancelled() must resolve within the test timeout")
            .expect("spawned cancellation task must not panic");
        assert!(flag, "flag must be visible once cancelled() resolves");
    }

    fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = Arc::clone(&n);
        (n, move || {
            n2.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn on_cancel_fires_on_self_cancel() {
        let c = CancelToken::root();
        let child = c.child();
        let (fired, waker) = counter();
        let _guard = child.on_cancel(waker);
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        child.cancel();
        assert_eq!(fired.load(Ordering::SeqCst), 1, "waker must fire on cancel");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn on_cancel_fires_on_ancestor_cancel() {
        // WHY: a consumer holding a CHILD token must be woken when a master
        // cancels — the down-propagation drain reaches the child's node.
        let master = CancelToken::root();
        let child = master.child();
        let (fired, waker) = counter();
        let _guard = child.on_cancel(waker);
        master.cancel();
        assert!(
            fired.load(Ordering::SeqCst) >= 1,
            "ancestor cancel must wake a descendant's sync waker"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn on_cancel_already_cancelled_fires_immediately() {
        let c = CancelToken::never();
        c.cancel();
        let (fired, waker) = counter();
        let _guard = c.on_cancel(waker);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            1,
            "registering on an already-cancelled token fires at once"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn on_cancel_guard_drop_unregisters() {
        let c = CancelToken::never();
        let (fired, waker) = counter();
        let guard = c.on_cancel(waker);
        drop(guard);
        c.cancel();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "a dropped guard must not fire on a later cancel"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn on_cancel_sibling_cancel_does_not_fire() {
        let parent = CancelToken::root();
        let a = parent.child();
        let b = parent.child();
        let (fired, waker) = counter();
        let _guard = a.on_cancel(waker);
        b.cancel();
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "a sibling cancel must not fire this token's waker"
        );
        assert!(!a.is_cancelled());
    }
}
