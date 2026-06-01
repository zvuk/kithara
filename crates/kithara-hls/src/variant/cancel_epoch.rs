use kithara_platform::{CancellationToken, RwLock};

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
    master: CancellationToken,
    current: RwLock<CancellationToken>,
}

impl CancelEpoch {
    pub(super) fn new(master: CancellationToken) -> Self {
        let current = RwLock::new(master.child_token());
        Self { master, current }
    }

    /// Cancel the current epoch token (variant deactivation).
    pub(super) fn cancel(&self) {
        self.current.lock_sync_read().cancel();
    }

    /// Clone the current epoch token — attached to every emitted `FetchCmd`.
    pub(super) fn handle(&self) -> CancellationToken {
        self.current.lock_sync_read().clone()
    }

    /// Rotate to a fresh child of `master` on re-activation.
    pub(super) fn rearm(&self) {
        let fresh = self.master.child_token();
        *self.current.lock_sync_write() = fresh;
    }
}
