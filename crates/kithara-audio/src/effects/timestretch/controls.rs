use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use portable_atomic::AtomicF32;

use super::stretch_kind::StretchBackendKind;

/// Single source of truth for live time-stretch control, shared (via `Arc`)
/// between the consumer/UI and the audio worker's effect chain. Replaces the
/// former split `playback_rate` / `tempo_ratio` mirror.
///
/// All fields are read each chunk by [`TimeStretchProcessor`] and may be
/// written at any time from the control thread; updates take effect on the
/// next processed chunk. See the crate `README.md` ("Live stretch controls")
/// for the speed-routing contract.
///
/// [`TimeStretchProcessor`]: super::processor::TimeStretchProcessor
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    speed: AtomicF32,
    keylock: AtomicBool,
    backend: AtomicU8,
}

impl StretchControls {
    /// Build a handle at `speed` (1.0 = normal), key-lock off, default backend.
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            speed: AtomicF32::new(speed),
            keylock: AtomicBool::new(false),
            backend: AtomicU8::new(StretchBackendKind::default().to_u8()),
        })
    }

    /// Playback speed (>1 faster). In tempo mode this drives the stretch
    /// factor; with key-lock off it is routed to the resampler instead.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed.load(Ordering::Relaxed)
    }

    /// Set the playback speed.
    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed, Ordering::Relaxed);
    }

    /// Whether key-lock (pitch-preserving tempo) is enabled.
    #[must_use]
    pub fn keylock(&self) -> bool {
        self.keylock.load(Ordering::Relaxed)
    }

    /// Enable/disable key-lock.
    pub fn set_keylock(&self, on: bool) {
        self.keylock.store(on, Ordering::Relaxed);
    }

    /// The selected time-stretch backend.
    #[must_use]
    pub fn backend(&self) -> StretchBackendKind {
        StretchBackendKind::from_u8(self.backend.load(Ordering::Relaxed))
    }

    /// Select the time-stretch backend.
    pub fn set_backend(&self, backend: StretchBackendKind) {
        self.backend.store(backend.to_u8(), Ordering::Relaxed);
    }
}
