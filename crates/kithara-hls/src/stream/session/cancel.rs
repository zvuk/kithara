use kithara_platform::{CancelToken, sync::RwLock};

use crate::variant::DispatchTokens;

/// Cancel hierarchy of one session: `root` → `fetch` → `lookahead`.
///
/// `fetch` covers every fetch the session issues and rotates on seek
/// ([`Self::rearm`]). `lookahead` is its child and covers only prefetches
/// past the owed window, so a variant transition can retire them
/// ([`Self::retire_lookahead`]) without touching owed work — while a seek's
/// rearm still burns both through the parent.
pub(super) struct SessionCancel {
    pub(super) root: CancelToken,
    fetch: RwLock<CancelToken>,
    lookahead: RwLock<CancelToken>,
}

impl SessionCancel {
    pub(super) fn new(root: CancelToken) -> Self {
        let fetch = root.child();
        let lookahead = RwLock::new(fetch.child());
        Self {
            root,
            fetch: RwLock::new(fetch),
            lookahead,
        }
    }

    delegate::delegate! {
        to self.root {
            #[call(cancel)]
            pub(super) fn abort(&self);
            pub(super) fn is_cancelled(&self) -> bool;
        }
    }

    pub(super) fn dispatch_tokens(&self) -> DispatchTokens {
        DispatchTokens {
            fetch: self.fetch.read().clone(),
            lookahead: self.lookahead.read().clone(),
        }
    }

    pub(super) fn retire_lookahead(&self) {
        self.lookahead.read().cancel();
        let next = self.fetch.read().child();
        *self.lookahead.write() = next;
    }

    pub(super) fn rearm(&self) {
        self.fetch.read().cancel();
        let fetch = self.root.child();
        *self.fetch.write() = fetch.clone();
        *self.lookahead.write() = fetch.child();
    }
}
