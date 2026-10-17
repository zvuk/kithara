use kithara_platform::{CancelToken, sync::RwLock};

use crate::variant::DispatchTokens;

/// Cancel hierarchy of one session: `root` → `fetch` → `lookahead`.
///
/// Exact size discovery rides `root`; `fetch` keeps immutable body fetches live
/// across seeks. Its `lookahead` child covers prefetches past the owed window,
/// which a variant transition can retire without cancelling owed bytes.
pub(super) struct SessionCancel {
    pub(super) root: CancelToken,
    fetch: CancelToken,
    lookahead: RwLock<CancelToken>,
}

impl SessionCancel {
    pub(super) fn new(root: CancelToken) -> Self {
        let fetch = root.child();
        let lookahead = RwLock::new(fetch.child());
        Self {
            root,
            fetch,
            lookahead,
        }
    }

    pub(super) fn dispatch_tokens(&self) -> DispatchTokens {
        DispatchTokens {
            metadata: self.root.clone(),
            fetch: self.fetch.clone(),
            lookahead: self.lookahead.read().clone(),
        }
    }

    pub(super) fn retire_lookahead(&self) {
        self.lookahead.read().cancel();
        let next = self.fetch.child();
        *self.lookahead.write() = next;
    }

    delegate::delegate! {
        to self.root {
            #[call(cancel)]
            pub(super) fn abort(&self);
            pub(super) fn is_cancelled(&self) -> bool;
        }
    }
}
