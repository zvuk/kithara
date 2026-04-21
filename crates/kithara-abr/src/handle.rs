use std::sync::{Arc, Weak, atomic::AtomicUsize};

use kithara_events::{AbrMode, EventBus};

use crate::{
    abr::Abr,
    controller::{AbrController, AbrPeerId},
    state::{AbrError, AbrState},
};

/// Clone-able handle returned by [`AbrController::register`].
///
/// Mirrors the shape of `PeerHandle` in `kithara-stream`: the consumer
/// attaches the track-scoped event bus with [`Self::with_bus`] and keeps
/// the handle alive for the lifetime of the peer.
#[derive(Clone)]
pub struct AbrHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    controller: Arc<AbrController>,
    peer_id: AbrPeerId,
    state: Option<Arc<AbrState>>,
    peer_weak: Weak<dyn Abr>,
}

impl AbrHandle {
    pub(crate) fn new(
        controller: Arc<AbrController>,
        peer_id: AbrPeerId,
        state: Option<Arc<AbrState>>,
        peer_weak: Weak<dyn Abr>,
    ) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                controller,
                peer_id,
                state,
                peer_weak,
            }),
        }
    }

    /// Attach the track-scoped event bus. Chains through to the peer's own
    /// `Abr::with_bus`, so `AbrController` can retrieve it later via
    /// `Abr::bus`.
    #[must_use]
    pub fn with_bus(self, bus: EventBus) -> Self {
        if let Some(peer) = self.inner.peer_weak.upgrade() {
            peer.with_bus(Some(bus));
        }
        self
    }

    #[must_use]
    pub fn peer_id(&self) -> AbrPeerId {
        self.inner.peer_id
    }

    #[must_use]
    pub fn state(&self) -> Option<&AbrState> {
        self.inner.state.as_deref()
    }

    #[must_use]
    pub fn controller(&self) -> &Arc<AbrController> {
        &self.inner.controller
    }

    /// Current variant index — `None` for peers without state.
    #[must_use]
    pub fn current_variant_index(&self) -> Option<usize> {
        self.inner.state.as_ref().map(|s| s.current_variant_index())
    }

    /// Shared atomic pointer to the current variant index. Hot-path readers
    /// can cache this and load with `Ordering::Acquire`.
    #[must_use]
    pub fn variant_index_handle(&self) -> Option<Arc<AtomicUsize>> {
        self.inner.state.as_ref().map(|s| s.variant_index_handle())
    }

    #[must_use]
    pub fn mode(&self) -> Option<AbrMode> {
        self.inner.state.as_ref().map(|s| s.mode())
    }

    /// Change mode.
    ///
    /// # Errors
    /// Returns [`AbrError::VariantOutOfBounds`] when `mode` is
    /// `AbrMode::Manual(idx)` and `idx` is not in the peer's variant list.
    pub fn set_mode(&self, mode: AbrMode) -> Result<(), AbrError> {
        match self.inner.state.as_ref() {
            Some(state) => {
                state.set_mode(mode)?;
                self.inner
                    .controller
                    .on_mode_changed(self.inner.peer_id, mode);
                Ok(())
            }
            None => Ok(()),
        }
    }

    #[must_use]
    pub fn max_bandwidth_bps(&self) -> Option<u64> {
        self.inner
            .state
            .as_ref()
            .and_then(|s| s.max_bandwidth_bps())
    }

    pub fn set_max_bandwidth_bps(&self, cap: Option<u64>) {
        if let Some(state) = self.inner.state.as_ref() {
            state.set_max_bandwidth_bps(cap);
            self.inner
                .controller
                .on_max_bandwidth_cap_changed(self.inner.peer_id, cap);
        }
    }

    /// Lock ABR (used during seek).
    pub fn lock(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            let before = state.lock_count();
            state.lock();
            if before == 0 {
                self.inner.controller.on_locked(self.inner.peer_id);
            }
        }
    }

    /// Release one lock level.
    pub fn unlock(&self) {
        if let Some(state) = self.inner.state.as_ref() {
            state.unlock();
            if state.lock_count() == 0 {
                self.inner.controller.on_unlocked(self.inner.peer_id);
            }
        }
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.inner.state.as_ref().is_some_and(|s| s.is_locked())
    }
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.controller.unregister(self.peer_id);
    }
}
