use std::sync::Arc;

use futures::future::select_all;
use tokio_util::sync::CancellationToken;

/// OR-combinator for cancellation tokens.
///
/// Fires when **any** source token is cancelled. No spawn — uses
/// sync polling for `is_cancelled()` and `select_all` for the
/// async `cancelled()` future.
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::CancelGroup;

    // ── Helpers ──────────────────────────────────────────────────

    /// Token descriptor for test parametrization.
    #[derive(Clone, Debug)]
    enum Src {
        Fresh,
        ChildOf(usize),
        PreCancelled,
    }

    /// What to cancel to trigger the group.
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

    // ── Sync parametrized tests ─────────────────────────────────

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

    // ── Async parametrized tests ────────────────────────────────

    macro_rules! async_cancel_tests {
        ($($name:ident: $spec:expr, $action:expr;)*) => {
            $(
                #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
                async fn $name() {
                    let s = build(&$spec);
                    let group2 = s.group.clone();
                    let handle = tokio::spawn(async move { group2.cancelled().await });

                    tokio::task::yield_now().await;

                    assert!(!s.group.is_cancelled(), "must not be cancelled before action");
                    fire(&$action, &s);

                    tokio::time::timeout(Duration::from_secs(2), handle)
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

    // ── Edge cases ──────────────────────────────────────────────

    #[kithara::test(tokio, timeout(Duration::from_secs(5)))]
    async fn cancelled_resolves_immediately_when_pre_cancelled() {
        let tok = CancellationToken::new();
        tok.cancel();
        let group = CancelGroup::new(vec![tok, CancellationToken::new()]);

        tokio::time::timeout(Duration::from_secs(1), group.cancelled())
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
        let result = tokio::time::timeout(Duration::from_millis(50), group.cancelled()).await;
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
}
