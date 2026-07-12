use std::sync::{
    Weak,
    atomic::{AtomicBool, AtomicU64},
};

use kithara_events::EventBus;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use super::throttle::EventThrottleCache;
use crate::{abr::Abr, state::AbrState};

/// Per-peer bookkeeping shared by tick orchestration, throttling, and the
/// incoherence watcher.
pub(crate) struct PeerEntry {
    pub(crate) peer_weak: Weak<dyn Abr>,
    pub(super) bus: Arc<RwLock<Option<EventBus>>>,
    pub(super) variants_registered_published: AtomicBool,
    pub(super) bytes_downloaded: AtomicU64,
    pub(super) incoherence_cancel: Mutex<Option<CancelToken>>,
    pub(super) last_variant_switch: Mutex<Option<(Instant, Duration)>>,
    pub(super) throttle: Mutex<EventThrottleCache>,
    pub(super) state: Option<Arc<AbrState>>,
}

impl PeerEntry {
    pub(super) fn bus(&self) -> Option<EventBus> {
        self.bus.read().clone()
    }
}
