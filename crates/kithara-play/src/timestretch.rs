use std::fmt;
use std::sync::Arc;

use kithara_audio::{StretchBackendKind, StretchControls};

/// Per-deck time-stretch control handle. One instance is created per deck at
/// the composition root and shared (via `Arc`) between the player and the UI,
/// making it the single owner of speed, key-lock, and backend selection.
///
/// It wraps the audio crate's [`StretchControls`], which is the same atomic
/// state the worker's effect chain reads each chunk — so key-lock and backend
/// changes apply live, mid-track, without a reload.
pub struct TimestretchControls {
    inner: Arc<StretchControls>,
}

impl fmt::Debug for TimestretchControls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimestretchControls")
            .field("speed", &self.speed())
            .field("keylock", &self.keylock())
            .field("backend", &self.backend())
            .finish()
    }
}

impl TimestretchControls {
    /// Build a handle at `speed` (1.0 = normal), key-lock off, default backend.
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            inner: StretchControls::new(speed),
        })
    }

    /// The shared controls the audio effect chain reads. Flow this into the
    /// resource config so every loaded track sees live changes.
    #[must_use]
    pub fn shared(&self) -> Arc<StretchControls> {
        Arc::clone(&self.inner)
    }

    /// Playback speed (>1 faster).
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.inner.speed()
    }

    /// Set the playback speed.
    pub fn set_speed(&self, speed: f32) {
        self.inner.set_speed(speed);
    }

    /// Enable/disable key-lock (pitch-preserving tempo). Applies live.
    pub fn set_keylock(&self, on: bool) {
        self.inner.set_keylock(on);
    }

    /// Whether key-lock is enabled.
    #[must_use]
    pub fn keylock(&self) -> bool {
        self.inner.keylock()
    }

    /// Select the time-stretch backend. Applies live.
    pub fn set_backend(&self, backend: StretchBackendKind) {
        self.inner.set_backend(backend);
    }

    /// The selected time-stretch backend.
    #[must_use]
    pub fn backend(&self) -> StretchBackendKind {
        self.inner.backend()
    }
}
