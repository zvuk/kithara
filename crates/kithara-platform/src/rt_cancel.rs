use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio_util::sync::{
    CancellationToken as AsyncToken, WaitForCancellationFuture, WaitForCancellationFutureOwned,
};

/// Real-time-safe cancellation token with correct hierarchy.
///
/// Pairs a [`tokio_util::sync::CancellationToken`] (the async `cancelled()`
/// side + the wider propagation tree) with a **chain of per-node cancel flags**
/// so the cancelled state can be read **lock-free** on the audio produce-core.
/// `tokio_util`'s own `is_cancelled()` takes the `TreeNode` `Mutex<Inner>`
/// (no atomic fast path), which traps under `RealtimeSanitizer` when read from
/// a `#[rtsan_forbid_blocking]` region.
///
/// This is the **single** cancellation token type across the workspace: the
/// async-only propagation network (downloader, net, assets, storage) and the
/// real-time produce-core both hold it. The lock-free flag is simply unused by
/// async-only consumers.
///
/// # Hierarchy
///
/// Each token owns its own flag and an `Arc` link to its parent's chain.
/// [`cancel`](CancellationToken::cancel) sets **only this token's** flag;
/// [`is_cancelled`](CancellationToken::is_cancelled) walks `self → root` and
/// returns `true` if any flag on the path is set. So:
/// - a parent / master `cancel()` is observed by every descendant (the
///   descendant's walk reaches the parent's flag), and
/// - cancelling a child or a sibling never marks the parent cancelled (the
///   parent's chain does not include the child's flag).
///
/// The walk is wait-free and bounded by tree depth (master → consumer is 2–3
/// nodes), never takes a lock, and is safe on the produce-core.
///
/// [`Clone`] yields the **same** identity (same flag node + same inner token),
/// like `tokio_util`'s clone. See `crates/kithara-platform/README.md`
/// "`CancellationToken`" and the `AGENTS.md` cancel-hierarchy contract.
///
/// `Default` mints a fresh **root** master (uncancelled, no parent). Owner site
/// only — all non-root tokens must come from
/// [`CancellationToken::child_token`].
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: AsyncToken,
    chain: Arc<ChainNode>,
}

/// One link in a token's ancestor chain: this node's own cancel flag plus an
/// `Arc` to the parent link (`None` at a root master).
#[derive(Default)]
struct ChainNode {
    flag: AtomicBool,
    parent: Option<Arc<Self>>,
}

impl CancellationToken {
    /// Derive a child token rooted at this token's chain.
    ///
    /// The inner token is a real `tokio_util` child (async propagation), and
    /// the child's flag links to this token's chain — so a parent / master
    /// `cancel()` is seen by the child's lock-free
    /// [`is_cancelled`](CancellationToken::is_cancelled), while the child's own
    /// `cancel()` never marks this parent cancelled.
    #[must_use]
    pub fn child_token(&self) -> Self {
        Self {
            inner: self.inner.child_token(),
            chain: Arc::new(ChainNode {
                flag: AtomicBool::new(false),
                parent: Some(Arc::clone(&self.chain)),
            }),
        }
    }

    /// Cancel this token and its descendants.
    ///
    /// `Release`-stores **this node's** flag **before** `inner.cancel()` so a
    /// thread that observes the inner async cancellation (or any later
    /// `Acquire` walk) is guaranteed to see the flag set — the flag can never
    /// lag the inner token. Descendants observe it through their own
    /// [`is_cancelled`](CancellationToken::is_cancelled) walk; ancestors and
    /// siblings do not (their chains do not include this node's flag).
    pub fn cancel(&self) {
        self.chain.flag.store(true, Ordering::Release);
        self.inner.cancel();
    }

    /// Lock-free cancelled read — the RT-safe path.
    ///
    /// Walks `self → root`, `Acquire`-loading each flag, and returns `true` at
    /// the first set one. Never touches the `tokio_util` `Mutex<Inner>`.
    /// Wait-free, bounded by tree depth. Pairs with the `Release` store in
    /// [`cancel`](CancellationToken::cancel).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        let mut node: &ChainNode = &self.chain;
        loop {
            if node.flag.load(Ordering::Acquire) {
                return true;
            }
            match &node.parent {
                Some(parent) => node = parent,
                None => return false,
            }
        }
    }

    delegate::delegate! {
        to self.inner {
            /// Future that resolves when this token (or an ancestor) is
            /// cancelled. The async side is unchanged `tokio_util`.
            pub fn cancelled(&self) -> WaitForCancellationFuture<'_>;
        }
        to self.inner.clone() {
            /// `'static` flavour of [`cancelled`](CancellationToken::cancelled).
            pub fn cancelled_owned(&self) -> WaitForCancellationFutureOwned;
        }
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;
    use tokio::{spawn, task, time as tokio_time};

    use super::CancellationToken;

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn fresh_token_not_cancelled() {
        let c = CancellationToken::default();
        assert!(!c.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cancel_sets_lock_free_flag() {
        let c = CancellationToken::default();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_cancel_does_not_cancel_parent() {
        // WHY: the hierarchy fix. Cancelling a child must set only the child's
        // flag, never the parent's. The old single-shared-atomic design failed
        // this: child.cancel() flipped the parent's is_cancelled().
        let parent = CancellationToken::default();
        let child = parent.child_token();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(
            !parent.is_cancelled(),
            "child cancel must not cancel the parent"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_cancel_does_not_cancel_sibling() {
        let parent = CancellationToken::default();
        let a = parent.child_token();
        let b = parent.child_token();
        a.cancel();
        assert!(a.is_cancelled());
        assert!(!b.is_cancelled(), "sibling cancel must stay independent");
        assert!(!parent.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn clone_shares_cancel_identity() {
        // WHY: Clone is the SAME token (same flag node), so cancelling a clone
        // is observed by the original — distinct from child_token().
        let token = CancellationToken::default();
        let twin = token.clone();
        twin.cancel();
        assert!(
            token.is_cancelled(),
            "clone shares identity with the original"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_observes_own_cancel() {
        let parent = CancellationToken::default();
        let child = parent.child_token();
        child.cancel();
        assert!(child.is_cancelled());
    }

    /// The critical lost-cancel guard: a worker token derived via
    /// `child_token()` must observe a master `cancel()` through its lock-free
    /// read, even though nobody calls the worker token's own `cancel()`.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn child_observes_parent_cancel_lock_free() {
        let master = CancellationToken::default();
        let worker = master.child_token();
        assert!(!worker.is_cancelled());

        let m = master.clone();
        std::thread::spawn(move || m.cancel())
            .join()
            .expect("cancel thread must not panic");

        assert!(
            worker.is_cancelled(),
            "worker child must observe master cancel via the chain walk"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn grandchild_observes_master_cancel_lock_free() {
        let master = CancellationToken::default();
        let mid = master.child_token();
        let leaf = mid.child_token();

        master.cancel();
        assert!(mid.is_cancelled());
        assert!(leaf.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn inner_async_cancelled_resolves_on_self_cancel() {
        let c = CancellationToken::default();
        let c2 = c.clone();
        let handle = spawn(async move { c2.cancelled().await });
        task::yield_now().await;

        c.cancel();

        tokio_time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancelled() must resolve within the test timeout")
            .expect("spawned cancellation task must not panic");
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn child_async_cancelled_resolves_on_parent_cancel() {
        let master = CancellationToken::default();
        let child = master.child_token();
        let fut = child.cancelled_owned();
        let handle = spawn(fut);
        task::yield_now().await;

        master.cancel();

        tokio_time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("child cancelled() must resolve on parent cancel")
            .expect("spawned task must not panic");
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn release_store_orders_before_inner_cancel() {
        // WHY: cancel() Release-stores the flag BEFORE inner.cancel(), so any
        // thread that observes the async cancellation must also see the
        // lock-free flag set. Resolving cancelled_owned() proves the inner
        // token fired; is_cancelled() must then already be true.
        let master = CancellationToken::default();
        let worker = master.child_token();
        let w = worker.clone();

        let handle = spawn(async move {
            w.cancelled_owned().await;
            w.is_cancelled()
        });
        task::yield_now().await;

        master.cancel();

        let flag_seen = tokio_time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancelled() must resolve within the test timeout")
            .expect("spawned task must not panic");
        assert!(
            flag_seen,
            "a thread that observed inner cancellation must also see the flag set"
        );
    }
}
