use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_audio::StretchBackendKind;
use kithara_platform::Mutex;

/// Per-deck time-stretch control handle. One instance is created per deck at
/// the composition root and shared (via `Arc`) between the player and the UI,
/// making it the single owner of key-lock and backend selection. Both apply at
/// the next track (re)load, not mid-track.
pub struct TimestretchControls {
    keylock: AtomicBool,
    backend: Mutex<StretchBackendKind>,
}

impl fmt::Debug for TimestretchControls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimestretchControls")
            .field("keylock", &self.keylock())
            .field("backend", &self.backend())
            .finish()
    }
}

impl TimestretchControls {
    /// Build a handle with key-lock off and the default backend.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            keylock: AtomicBool::new(false),
            backend: Mutex::new(StretchBackendKind::default()),
        })
    }

    /// Enable/disable key-lock (pitch-preserving tempo).
    pub fn set_keylock(&self, on: bool) {
        self.keylock.store(on, Ordering::Relaxed);
    }

    /// Whether key-lock is enabled for newly loaded tracks.
    #[must_use]
    pub fn keylock(&self) -> bool {
        self.keylock.load(Ordering::Relaxed)
    }

    /// Select the time-stretch backend for tempo mode.
    pub fn set_backend(&self, backend: StretchBackendKind) {
        *self.backend.lock_sync() = backend;
    }

    /// The time-stretch backend used for newly loaded tracks in tempo mode.
    #[must_use]
    pub fn backend(&self) -> StretchBackendKind {
        *self.backend.lock_sync()
    }
}
