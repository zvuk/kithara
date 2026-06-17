use std::fmt;

use super::token::CancelToken;

/// A cancellation scope owned by a subsystem.
///
/// [`new`](CancelScope::new) is the canonical replacement for the legacy
/// `cancel.unwrap_or_default()` fallback — the `Option<CancelToken>` parent picks
/// the branch:
/// - **composed** (`Some(parent)`): the scope's token is a child of the parent,
///   so a parent/master cancel reaches this subtree.
/// - **standalone** (`None`): the scope's token is itself a fresh root
///   ([`CancelToken::root`]); nothing above can cancel it.
///
/// Either way, the inner children handed out by [`token`](CancelScope::token)
/// derive from one node, and [`cancel`](CancelScope::cancel) cancels exactly that
/// subtree. `Drop` is **passive**: dropping a scope does not cancel its subtree;
/// teardown is an explicit `cancel()` (so a composed scope never cancels a token
/// it was handed from above).
pub struct CancelScope {
    token: CancelToken,
}

impl CancelScope {
    /// Build a scope from an optional parent token (the composed/standalone
    /// seam). This is a domain constructor, not a generic `From` conversion: the
    /// branch on `parent` decides ownership of the subtree root.
    #[must_use]
    pub fn new(parent: Option<CancelToken>) -> Self {
        let token = parent.map_or_else(CancelToken::root, |parent| parent.child());
        Self { token }
    }

    /// Cancel this scope's subtree (the node from which inner children derive).
    pub fn cancel(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The scope's token. Clone it for the subsystem's own subtree (`.child()`
    /// for per-task/per-fetch tokens).
    #[must_use]
    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }
}

impl fmt::Debug for CancelScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancelScope")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kithara_test_utils::kithara;

    use super::CancelScope;
    use crate::common::cancel::CancelToken;

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn standalone_scope_owns_uncancelled_root() {
        let scope = CancelScope::new(None);
        assert!(!scope.is_cancelled());
        assert!(!scope.token().is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn standalone_cancel_cancels_own_subtree() {
        let scope = CancelScope::new(None);
        let token = scope.token();
        scope.cancel();
        assert!(scope.is_cancelled());
        assert!(token.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn composed_scope_observes_parent_cancel() {
        let parent = CancelToken::never();
        let scope = CancelScope::new(Some(parent.clone()));
        let token = scope.token();
        assert!(!scope.is_cancelled());
        parent.cancel();
        assert!(scope.is_cancelled());
        assert!(token.is_cancelled());
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn composed_scope_cancel_does_not_cancel_parent() {
        // The single sanctioned semantic of the wave: a scope cancels only its
        // own subtree, never the token it was handed from above.
        let parent = CancelToken::never();
        let scope = CancelScope::new(Some(parent.clone()));
        scope.cancel();
        assert!(scope.is_cancelled());
        assert!(
            !parent.is_cancelled(),
            "scope.cancel() must not cancel the parent token it was handed"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn drop_is_passive() {
        // Dropping a scope must NOT cancel its subtree — teardown is explicit.
        let parent = CancelToken::never();
        let live = {
            let scope = CancelScope::new(Some(parent.clone()));
            scope.token()
        };
        assert!(
            !live.is_cancelled() && !parent.is_cancelled(),
            "dropping a scope must not cancel anything"
        );
    }
}
