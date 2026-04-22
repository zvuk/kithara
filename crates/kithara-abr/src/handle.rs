use std::sync::{Arc, atomic::AtomicUsize};

use kithara_events::{AbrMode, EventBus};
use kithara_platform::RwLock;

use crate::{
    controller::{AbrController, AbrPeerId},
    state::{AbrError, AbrState},
};

/// Clone-able handle returned by [`AbrController::register`].
///
/// Mirrors the shape of `PeerHandle` in `kithara-stream`: the consumer
/// attaches the track-scoped event bus with [`Self::with_bus`] and keeps
/// the handle alive for the lifetime of the peer. The bus lives inside
/// the handle — peers stay free of event-bus plumbing.
#[derive(Clone)]
pub struct AbrHandle {
    inner: Arc<HandleInner>,
}

pub(crate) struct HandleInner {
    pub(crate) controller: Arc<AbrController>,
    pub(crate) peer_id: AbrPeerId,
    pub(crate) state: Option<Arc<AbrState>>,
    pub(crate) bus: Arc<RwLock<Option<EventBus>>>,
}

impl AbrHandle {
    pub(crate) fn new(
        controller: Arc<AbrController>,
        peer_id: AbrPeerId,
        state: Option<Arc<AbrState>>,
        bus: Arc<RwLock<Option<EventBus>>>,
    ) -> Self {
        Self {
            inner: Arc::new(HandleInner {
                controller,
                peer_id,
                state,
                bus,
            }),
        }
    }

    /// Attach the track-scoped event bus. Stored directly on the handle;
    /// the controller reads it through the shared `Arc` when publishing.
    #[must_use]
    pub fn with_bus(self, bus: EventBus) -> Self {
        *self.inner.bus.lock_sync_write() = Some(bus);
        self
    }

    #[must_use]
    pub fn peer_id(&self) -> AbrPeerId {
        self.inner.peer_id
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
