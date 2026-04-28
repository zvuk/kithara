use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU64},
};

use kithara_events::EventBus;
use kithara_platform::{
    Mutex, RwLock,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

use super::throttle::EventThrottleCache;
use crate::{abr::Abr, state::AbrState};

/// Per-peer bookkeeping shared by tick orchestration, throttling, and the
/// incoherence watcher.
pub(super) struct PeerEntry {
    pub(super) bus: Arc<RwLock<Option<EventBus>>>,
    pub(super) variants_registered_published: AtomicBool,
    pub(super) warmup_completed: AtomicBool,
    pub(super) bytes_downloaded: AtomicU64,
    pub(super) incoherence_cancel: Mutex<Option<CancellationToken>>,
    pub(super) last_variant_switch: Mutex<Option<(Instant, Duration)>>,
    pub(super) throttle: Mutex<EventThrottleCache>,
    pub(super) state: Option<Arc<AbrState>>,
    pub(super) peer_weak: Weak<dyn Abr>,
}

impl PeerEntry {
    pub(super) fn bus(&self) -> Option<EventBus> {
        self.bus.lock_sync_read().clone()
    }
}
