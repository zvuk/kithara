use std::{ops::BitOr, sync::Arc};

use futures::future::select_all;
use tokio_util::sync::CancellationToken;

/// OR-combinator for cancellation tokens.
///
/// Fires when **any** source token is cancelled. No spawn — uses
/// sync polling for `is_cancelled()` and `select_all` for the
/// async `cancelled()` future.
///
/// Supports composition via `|`:
/// ```ignore
/// let cancel = token_a | token_b;
/// let cancel = group | extra_token;
/// let cancel = group1 | group2;
/// ```
#[derive(Clone)]
pub struct CancelGroup {
    sources: Arc<[CancellationToken]>,
}

impl CancelGroup {
    #[must_use]
    pub fn new(sources: Vec<CancellationToken>) -> Self {
        Self {
            sources: sources.into(),
        }
    }

    /// Returns `true` if both groups share the same underlying source array.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.sources, &other.sources)
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.sources.iter().any(CancellationToken::is_cancelled)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let futs: Vec<_> = self
            .sources
            .iter()
            .map(|s| Box::pin(s.cancelled()))
            .collect();
        if futs.is_empty() {
            std::future::pending::<()>().await;
            return;
        }
        select_all(futs).await;
    }

    fn tokens(&self) -> &[CancellationToken] {
        &self.sources
    }
}

// From

impl From<CancellationToken> for CancelGroup {
    fn from(token: CancellationToken) -> Self {
        Self::new(vec![token])
    }
}

impl From<Vec<CancellationToken>> for CancelGroup {
    fn from(tokens: Vec<CancellationToken>) -> Self {
        Self::new(tokens)
    }
}

// BitOr: group | group

impl BitOr for CancelGroup {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        let mut tokens = self.tokens().to_vec();
        tokens.extend_from_slice(rhs.tokens());
        Self::new(tokens)
    }
}

// BitOr: group | token

impl BitOr<CancellationToken> for CancelGroup {
    type Output = Self;

    fn bitor(self, rhs: CancellationToken) -> Self {
        let mut tokens = self.tokens().to_vec();
        tokens.push(rhs);
        Self::new(tokens)
    }
}

// BitOr: token | group

impl BitOr<CancelGroup> for CancellationToken {
    type Output = CancelGroup;

    fn bitor(self, rhs: CancelGroup) -> CancelGroup {
        let mut tokens = vec![self];
        tokens.extend_from_slice(rhs.tokens());
        CancelGroup::new(tokens)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;
    use tokio::{spawn, task, time as tokio_time};
    use tokio_util::sync::CancellationToken;

    use super::CancelGroup;

    // Helpers

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
        sources: Vec<CancellationToken>,
        parents: Vec<CancellationToken>,
    }

    fn build(spec: &[Src]) -> Setup {
        let mut parents: Vec<CancellationToken> = Vec::new();
        let mut sources: Vec<CancellationToken> = Vec::new();

        for s in spec {
            match s {
                Src::Fresh => sources.push(CancellationToken::new()),
                Src::ChildOf(idx) => {
                    while parents.len() <= *idx {
                        parents.push(CancellationToken::new());
                    }
                    sources.push(parents[*idx].child_token());
                }
                Src::PreCancelled => {
                    let tok = CancellationToken::new();
                    tok.cancel();
                    sources.push(tok);
                }
            }
        }

        let group = CancelGroup::new(sources.clone());
        Setup {
            group,
            sources,
            parents,
        }
    }

    fn fire(act: &Act, s: &Setup) {
        match act {
            Act::Source(i) => s.sources[*i].cancel(),
            Act::Parent(i) => s.parents[*i].cancel(),
            Act::None => {}
        }
    }

    // Sync parametrized tests

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

    // Async parametrized tests

    macro_rules! async_cancel_tests {
        ($($name:ident: $spec:expr, $action:expr;)*) => {
            $(
                #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
                async fn $name() {
                    let s = build(&$spec);
                    let group2 = s.group.clone();
                    let handle = spawn(async move { group2.cancelled().await });

                    task::yield_now().await;

                    assert!(!s.group.is_cancelled(), "must not be cancelled before action");
                    fire(&$action, &s);

                    tokio_time::timeout(Duration::from_secs(2), handle)
                        .await
                        .expect("cancelled() must resolve within timeout")
                        .expect("task must not panic");
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

    // Edge cases

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn cancelled_resolves_immediately_when_pre_cancelled() {
        let tok = CancellationToken::new();
        tok.cancel();
        let group = CancelGroup::new(vec![tok, CancellationToken::new()]);

        tokio_time::timeout(Duration::from_secs(1), group.cancelled())
            .await
            .expect("cancelled() must return immediately for pre-cancelled source");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn empty_group_is_not_cancelled() {
        let group = CancelGroup::new(vec![]);
        assert!(!group.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn empty_group_cancelled_never_resolves() {
        let group = CancelGroup::new(vec![]);
        let result = tokio_time::timeout(Duration::from_millis(50), group.cancelled()).await;
        assert!(
            result.is_err(),
            "cancelled() on empty group must not resolve"
        );
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn clone_observes_same_cancellation() {
        let tok = CancellationToken::new();
        let group = CancelGroup::new(vec![tok.clone()]);
        let cloned = group.clone();

        tok.cancel();
        assert!(group.is_cancelled());
        assert!(cloned.is_cancelled());
    }

    // BitOr composition tests

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn token_bitor_token() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let group = CancelGroup::from(a.clone()) | b.clone();

        assert!(!group.is_cancelled());
        a.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_bitor_token() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let group = CancelGroup::from(a.clone()) | b.clone();

        assert!(!group.is_cancelled());
        b.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn token_bitor_group() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let group = a.clone() | CancelGroup::from(b.clone());

        assert!(!group.is_cancelled());
        a.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn group_bitor_group() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let g1 = CancelGroup::from(a.clone());
        let g2 = CancelGroup::from(b.clone());
        let merged = g1 | g2;

        assert!(!merged.is_cancelled());
        b.cancel();
        assert!(merged.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn chained_bitor() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let c = CancellationToken::new();
        let group = CancelGroup::from(a.clone()) | b.clone() | c.clone();

        assert!(!group.is_cancelled());
        c.cancel();
        assert!(group.is_cancelled());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn bitor_async_cancelled() {
        let a = CancellationToken::new();
        let b = CancellationToken::new();
        let group = CancelGroup::from(a.clone()) | b.clone();

        let g2 = group.clone();
        let handle = spawn(async move { g2.cancelled().await });
        task::yield_now().await;

        assert!(!group.is_cancelled());
        b.cancel();

        tokio_time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancelled() must resolve")
            .expect("task must not panic");
    }
}
