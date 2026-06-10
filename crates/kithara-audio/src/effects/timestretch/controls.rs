use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use std::sync::atomic::{AtomicBool, AtomicU8};

use portable_atomic::AtomicF32;

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use super::stretch_kind::StretchBackendKind;

/// Single source of truth for live playback-speed control, shared (via `Arc`)
/// between the consumer/UI and the audio worker's effect chain. Replaces the
/// former split `playback_rate` / `tempo_ratio` mirror.
///
/// Key-lock and backend selection exist only when a stretch backend is
/// compiled in (`stretch-signalsmith` / `stretch-bungee`, native targets);
/// without one the chain is resampler-first and pitch follows speed.
///
/// All fields are read each chunk by the effect chain and may be written at
/// any time from the control thread; updates take effect on the next
/// processed chunk. See the crate `README.md` ("Live stretch controls")
/// for the speed-routing contract.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    /// Shared with the resampler directly when no stretch backend is
    /// compiled in; with one, `TimeStretchProcessor` reads it per chunk.
    speed: Arc<AtomicF32>,
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    keylock: AtomicBool,
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    backend: AtomicU8,
}

impl StretchControls {
    /// Build a handle at `speed` (1.0 = normal), key-lock off, default backend.
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            speed: Arc::new(AtomicF32::new(speed)),
            #[cfg(all(
                not(target_arch = "wasm32"),
                any(feature = "stretch-signalsmith", feature = "stretch-bungee")
            ))]
            keylock: AtomicBool::new(false),
            #[cfg(all(
                not(target_arch = "wasm32"),
                any(feature = "stretch-signalsmith", feature = "stretch-bungee")
            ))]
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

    /// The speed atomic itself, for chains where the resampler follows the
    /// speed directly (no stretch backend compiled in).
    #[cfg(not(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    )))]
    pub(crate) fn speed_shared(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.speed)
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
impl StretchControls {
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
