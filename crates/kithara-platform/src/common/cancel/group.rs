use std::{
    fmt,
    future::Future,
    ops::BitOr,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
};

use super::{
    token::{CancelToken, CancelWakerGuard},
    wait::Cancelled,
};
use crate::sync::Arc;

/// OR-combinator for cancellation tokens.
///
/// Fires when **any** source token is cancelled. No spawn — `is_cancelled()`
/// polls each source, and [`cancelled`](CancelGroup::cancelled) is a single
/// future that parks one slot on every source (no per-source boxed futures).
///
/// Supports composition via `|`:
/// ```ignore
/// let cancel = token_a | token_b;
/// let cancel = group | extra_token;
/// let cancel = group1 | group2;
/// ```
#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(get, vis = "")]
pub struct CancelGroup {
    #[field(get = tokens)]
    sources: Arc<[CancelToken]>,
}

impl CancelGroup {
    #[must_use]
    pub fn new(sources: Vec<CancelToken>) -> Self {
        Self {
            sources: sources.into(),
        }
    }

    /// Future resolving once any source is cancelled. An empty group never
    /// resolves. One future with a slot per source — dropping it unregisters
    /// every slot (cancel-safe in `select!`).
    #[must_use]
    pub fn cancelled(&self) -> GroupCancelled<'_> {
        GroupCancelled {
            sources: &self.sources,
            slots: Vec::new(),
            done: false,
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.sources.iter().any(CancelToken::is_cancelled)
    }

    /// Register a synchronous waker that fires at most once when any source is
    /// cancelled. Retain all returned guards for the registration lifetime; an
    /// empty group returns no guards.
    #[must_use = "dropping the guards immediately unregisters the cancel waker"]
    pub fn on_cancel<F>(&self, waker: F) -> Vec<CancelWakerGuard>
    where
        F: Fn() + Send + Sync + 'static,
    {
        if self.sources.is_empty() {
            return Vec::new();
        }

        let shared = Arc::new((AtomicBool::new(false), waker));
        self.sources
            .iter()
            .map(|source| {
                let shared = Arc::clone(&shared);
                source.on_cancel(move || {
                    if !shared.0.swap(true, Ordering::AcqRel) {
                        (shared.1)();
                    }
                })
            })
            .collect()
    }
}

impl fmt::Debug for CancelGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelGroup")
            .field("sources", &self.sources.len())
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Identity comparison: two groups are equal iff they share the same underlying
/// source array.
impl PartialEq for CancelGroup {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sources, &other.sources)
    }
}

impl From<CancelToken> for CancelGroup {
    fn from(token: CancelToken) -> Self {
        Self::new(vec![token])
    }
}

impl From<Vec<CancelToken>> for CancelGroup {
    fn from(tokens: Vec<CancelToken>) -> Self {
        Self::new(tokens)
    }
}

impl BitOr for CancelGroup {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        let mut tokens = self.tokens().to_vec();
        tokens.extend_from_slice(rhs.tokens());
        Self::new(tokens)
    }
}

impl BitOr<CancelToken> for CancelGroup {
    type Output = Self;

    fn bitor(self, rhs: CancelToken) -> Self {
        let mut tokens = self.tokens().to_vec();
        tokens.push(rhs);
        Self::new(tokens)
    }
}

impl BitOr<CancelGroup> for CancelToken {
    type Output = CancelGroup;

    fn bitor(self, rhs: CancelGroup) -> CancelGroup {
        let mut tokens = vec![self];
        tokens.extend_from_slice(rhs.tokens());
        CancelGroup::new(tokens)
    }
}

/// Future returned by [`CancelGroup::cancelled`]. `Unpin`; parks one
/// `cancelled()` future per source and resolves when any fires.
pub struct GroupCancelled<'a> {
    sources: &'a [CancelToken],
    slots: Vec<Cancelled<'a>>,
    done: bool,
}

impl Future for GroupCancelled<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let me = self.get_mut();
        if me.done {
            return Poll::Ready(());
        }
        if me.slots.is_empty() {
            if me.sources.is_empty() {
                return Poll::Pending;
            }
            me.slots = me.sources.iter().map(CancelToken::cancelled).collect();
        }
        for slot in &mut me.slots {
            if Pin::new(slot).poll(cx).is_ready() {
                me.done = true;
                me.slots.clear();
                return Poll::Ready(());
            }
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use kithara_test_utils::kithara;

    use super::CancelGroup;
    use crate::{common::cancel::CancelToken, sync::Arc};

    fn on_cancel_counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&count);
        (count, move || {
            callback_count.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[derive(Clone, Debug)]
    enum Src {
        Fresh,
        ChildOf(usize),
        PreCancelled,
    }

    #[derive(Clone, Debug)]
    enum Act {
        Source(usize),
        Parent(usize),
        None,
    }

    struct Setup {
        group: CancelGroup,
        parents: Vec<CancelToken>,
        sources: Vec<CancelToken>,
    }

    fn build(spec: &[Src]) -> Setup {
        let mut parents: Vec<CancelToken> = Vec::new();
        let mut sources: Vec<CancelToken> = Vec::new();

        for s in spec {
            match s {
                Src::Fresh => sources.push(CancelToken::never()),
                Src::ChildOf(idx) => {
                    while parents.len() <= *idx {
                        parents.push(CancelToken::root());
                    }
                    sources.push(parents[*idx].child());
                }
                Src::PreCancelled => {
                    let tok = CancelToken::never();
                    tok.cancel();
                    sources.push(tok);
                }
            }
        }

        let group = CancelGroup::new(sources.clone());
        Setup {
            group,
            parents,
            sources,
        }
    }

    fn fire(act: &Act, s: &Setup) {
        match act {
            Act::Source(i) => s.sources[*i].cancel(),
            Act::Parent(i) => s.parents[*i].cancel(),
            Act::None => {}
        }
    }

    macro_rules! sync_cancel_tests {
        ($($name:ident: $spec:expr, $action:expr, $expected:expr;)*) => {
            $(
                #[kithara::test(timeout(Duration::from_secs(5)))]
                fn $name() {
                    let s = build(&$spec);
                    fire(&$action, &s);
                    assert_eq!(s.group.is_cancelled(), $expected);
                }
            )*
        }
    }

    sync_cancel_tests! {
        two_fresh_cancel_first:
            [Src::Fresh, Src::Fresh], Act::Source(0), true;
        two_fresh_cancel_second:
            [Src::Fresh, Src::Fresh], Act::Source(1), true;
        single_cancel:
            [Src::Fresh], Act::Source(0), true;
        two_fresh_no_cancel:
            [Src::Fresh, Src::Fresh], Act::None, false;
        pre_cancelled_plus_fresh:
            [Src::PreCancelled, Src::Fresh], Act::None, true;
        fresh_and_child_cancel_fresh:
            [Src::Fresh, Src::ChildOf(0)], Act::Source(0), true;
        fresh_and_child_cancel_parent:
            [Src::Fresh, Src::ChildOf(0)], Act::Parent(0), true;
        two_children_same_parent_cancel_parent:
            [Src::ChildOf(0), Src::ChildOf(0)], Act::Parent(0), true;
        two_children_diff_parents_cancel_first:
            [Src::ChildOf(0), Src::ChildOf(1)], Act::Parent(0), true;
        two_children_diff_parents_cancel_second:
            [Src::ChildOf(0), Src::ChildOf(1)], Act::Parent(1), true;
        two_children_diff_parents_no_cancel:
            [Src::ChildOf(0), Src::ChildOf(1)], Act::None, false;
        mixed_with_pre_cancelled:
            [Src::Fresh, Src::ChildOf(0), Src::PreCancelled], Act::None, true;
    }

    macro_rules! async_cancel_tests {
        ($($name:ident: $spec:expr, $action:expr;)*) => {
            $(
                #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
                async fn $name() {
                    let s = build(&$spec);
                    let group2 = s.group.clone();
                    let handle = crate::tokio::task::spawn(async move { group2.cancelled().await });

                    crate::tokio::task::yield_now().await;

                    assert!(!s.group.is_cancelled(), "must not be cancelled before action");
                    fire(&$action, &s);

                    crate::time::timeout(Duration::from_secs(2), handle)
                        .await
                        .expect("BUG: cancelled() must resolve within the test timeout")
                        .expect("BUG: spawned cancellation task must not panic");
                }
            )*
        }
    }

    async_cancel_tests! {
        async_two_fresh_cancel_first:
            [Src::Fresh, Src::Fresh], Act::Source(0);
        async_two_fresh_cancel_second:
            [Src::Fresh, Src::Fresh], Act::Source(1);
        async_single_cancel:
            [Src::Fresh], Act::Source(0);
        async_fresh_and_child_cancel_parent:
            [Src::Fresh, Src::ChildOf(0)], Act::Parent(0);
        async_two_children_same_parent:
            [Src::ChildOf(0), Src::ChildOf(0)], Act::Parent(0);
        async_two_children_diff_parents_cancel_first:
            [Src::ChildOf(0), Src::ChildOf(1)], Act::Parent(0);
        async_two_children_diff_parents_cancel_second:
            [Src::ChildOf(0), Src::ChildOf(1)], Act::Parent(1);
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn cancelled_resolves_immediately_when_pre_cancelled() {
        let tok = CancelToken::never();
        tok.cancel();
        let group = CancelGroup::new(vec![tok, CancelToken::never()]);

        crate::time::timeout(Duration::from_secs(1), group.cancelled())
            .await
            .expect("BUG: cancelled() must return immediately for a pre-cancelled source");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn empty_group_is_not_cancelled() {
        let group = CancelGroup::new(vec![]);
        assert!(!group.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn empty_group_cancelled_never_resolves() {
        let group = CancelGroup::new(vec![]);
        let result = crate::time::timeout(Duration::from_millis(50), group.cancelled()).await;
        assert!(
            result.is_err(),
            "cancelled() on empty group must not resolve"
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn clone_observes_same_cancellation() {
        let tok = CancelToken::never();
        let group = CancelGroup::new(vec![tok.clone()]);
        let cloned = group.clone();

        tok.cancel();
        assert!(group.is_cancelled());
        assert!(cloned.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn ptr_eq_via_partial_eq() {
        // A clone shares the source array, a rebuilt group does not.
        let tok = CancelToken::never();
        let group = CancelGroup::from(tok.clone());
        let cloned = group.clone();
        let rebuilt = CancelGroup::from(tok);
        assert_eq!(group, cloned);
        assert_ne!(group, rebuilt);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn token_bitor_token() {
        let a = CancelToken::never();
        let b = CancelToken::never();
        let group = CancelGroup::from(a.clone()) | b;

        assert!(!group.is_cancelled());
        a.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_bitor_token() {
        let a = CancelToken::never();
        let b = CancelToken::never();
        let group = CancelGroup::from(a) | b.clone();

        assert!(!group.is_cancelled());
        b.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn token_bitor_group() {
        let a = CancelToken::never();
        let b = CancelToken::never();
        let group = a.clone() | CancelGroup::from(b);

        assert!(!group.is_cancelled());
        a.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_bitor_group() {
        let a = CancelToken::never();
        let b = CancelToken::never();
        let g1 = CancelGroup::from(a);
        let g2 = CancelGroup::from(b.clone());
        let merged = g1 | g2;

        assert!(!merged.is_cancelled());
        b.cancel();
        assert!(merged.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn chained_bitor() {
        let a = CancelToken::never();
        let b = CancelToken::never();
        let c = CancelToken::never();
        let group = CancelGroup::from(a) | b | c.clone();

        assert!(!group.is_cancelled());
        c.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn bitor_async_cancelled() {
        use crate::tokio::task;

        let a = CancelToken::never();
        let b = CancelToken::never();
        let group = CancelGroup::from(a.clone()) | b.clone();

        let g2 = group.clone();
        let handle = task::spawn(async move { g2.cancelled().await });
        task::yield_now().await;

        assert!(!group.is_cancelled());
        b.cancel();

        crate::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("BUG: cancelled() must resolve once one source has cancelled")
            .expect("BUG: spawned task awaiting cancellation must not panic");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_on_cancel_fires_once_for_each_source() {
        for [first, second] in [[0, 1], [1, 0]] {
            let tokens = [CancelToken::never(), CancelToken::never()];
            let group = CancelGroup::new(tokens.to_vec());
            let (count, callback) = on_cancel_counter();
            let _guards = group.on_cancel(callback);

            tokens[first].cancel();
            tokens[second].cancel();
            tokens[first].cancel();

            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_on_cancel_fires_once_for_duplicate_and_pre_cancelled_sources() {
        let duplicate = CancelToken::never();
        let duplicate_group = CancelGroup::new(vec![duplicate.clone(), duplicate.clone()]);
        let (duplicate_count, duplicate_callback) = on_cancel_counter();
        let _duplicate_guards = duplicate_group.on_cancel(duplicate_callback);
        duplicate.cancel();
        assert_eq!(duplicate_count.load(Ordering::SeqCst), 1);

        let pre_cancelled = CancelToken::never();
        pre_cancelled.cancel();
        let live = CancelToken::never();
        let pre_cancelled_group = CancelGroup::new(vec![pre_cancelled, live.clone()]);
        let (pre_cancelled_count, pre_cancelled_callback) = on_cancel_counter();
        let _pre_cancelled_guards = pre_cancelled_group.on_cancel(pre_cancelled_callback);
        assert_eq!(pre_cancelled_count.load(Ordering::SeqCst), 1);
        live.cancel();
        assert_eq!(pre_cancelled_count.load(Ordering::SeqCst), 1);
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_on_cancel_empty_and_dropped_guards_do_not_fire() {
        let (empty_count, empty_callback) = on_cancel_counter();
        let empty_guards = CancelGroup::new(vec![]).on_cancel(empty_callback);
        assert!(empty_guards.is_empty());
        assert_eq!(empty_count.load(Ordering::SeqCst), 0);

        let tokens = [CancelToken::never(), CancelToken::never()];
        let group = CancelGroup::new(tokens.to_vec());
        let (dropped_count, dropped_callback) = on_cancel_counter();
        let guards = group.on_cancel(dropped_callback);
        drop(guards);
        tokens[0].cancel();
        tokens[1].cancel();
        assert_eq!(dropped_count.load(Ordering::SeqCst), 0);
    }

    #[kithara::test(native, timeout(Duration::from_secs(5)))]
    fn group_on_cancel_fires_once_when_sources_cancel_concurrently() {
        use std::sync::Barrier;

        let tokens = [CancelToken::never(), CancelToken::never()];
        let group = CancelGroup::new(tokens.to_vec());
        let (count, callback) = on_cancel_counter();
        let _guards = group.on_cancel(callback);
        let barrier = Arc::new(Barrier::new(3));

        std::thread::scope(|scope| {
            for token in &tokens {
                let token = token.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    token.cancel();
                });
            }
            barrier.wait();
        });

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
