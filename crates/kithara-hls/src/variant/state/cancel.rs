use kithara_platform::{CancelToken, sync::RwLock};

/// The variant's cancel epoch: the track-level parent token and the rotating
/// per-activation child.
///
/// Cancel hierarchy: `master` is the per-track parent created by `HlsPeer`
/// (itself a child of the consumer-top master cancel); `current` is a child of
/// `master`, rotated on every re-activation via [`rearm`](Self::rearm). A
/// cross-codec `commit_variant_switch` may flip from `v_old` to `v_new` and
/// back to `v_old`, and the second activation of `v_old` must dispatch fetches
/// under a *live* cancel — hence the rearm-on-activation rotation.
pub(super) struct CancelEpoch {
    master: CancelToken,
    current: RwLock<CancelToken>,
}

impl CancelEpoch {
    pub(super) fn new(master: CancelToken) -> Self {
        let current = RwLock::new(master.child());
        Self { master, current }
    }

    delegate::delegate! {
        to self.current.read() {
            /// Cancel the current epoch token (variant deactivation).
            pub(super) fn cancel(&self);
            /// Clone the current epoch token — attached to every emitted `FetchCmd`.
            #[call(clone)]
            pub(super) fn handle(&self) -> CancelToken;
        }
    }

    /// Rotate to a fresh child of `master` on re-activation.
    pub(super) fn rearm(&self) {
        let fresh = self.master.child();
        *self.current.write() = fresh;
    }
}
