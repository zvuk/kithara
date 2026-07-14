use std::sync::atomic::Ordering;

use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
use portable_atomic::AtomicF32;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
use {
    kithara_stretch::StretchKind,
    std::sync::atomic::{AtomicBool, AtomicU8},
};

use crate::region::RegionPlan;

/// Live playback-speed control shared by the caller and the effect chain.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    /// Read per chunk by `TimeStretchProcessor` when a backend is compiled in.
    speed: Arc<AtomicF32>,
    region_plan: ArcSwapOption<RegionPlan>,
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(feature = "stretch-signalsmith", feature = "stretch-bungee")
    ))]
    engine: EngineControls,
}

/// Stretch-engine controls present only when a backend is compiled in.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
#[derive(Debug)]
struct EngineControls {
    keylock: AtomicBool,
    backend: AtomicU8,
}

impl StretchControls {
    /// Build a handle at `speed` (1.0 = normal) with key-lock off.
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            speed: Arc::new(AtomicF32::new(speed)),
            region_plan: ArcSwapOption::const_empty(),
            #[cfg(all(
                not(target_arch = "wasm32"),
                any(feature = "stretch-signalsmith", feature = "stretch-bungee")
            ))]
            engine: EngineControls {
                keylock: AtomicBool::new(false),
                backend: AtomicU8::new(u8::from(StretchKind::default())),
            },
        })
    }

    delegate::delegate! {
        to self.region_plan {
            /// The active region-stretch plan, if any.
            #[must_use]
            #[call(load_full)]
            pub fn region_plan(&self) -> Option<Arc<RegionPlan>>;
            /// Install or clear the region-stretch plan; picked up on the next chunk.
            #[call(store)]
            pub fn set_region_plan(&self, plan: Option<Arc<RegionPlan>>);
        }
    }
    /// Set the playback speed.
    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed, Ordering::Relaxed);
    }

    /// Playback speed (>1 faster). When stretch is compiled in this drives the
    /// stretch slot; otherwise it is retained as control state only.
    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed.load(Ordering::Relaxed)
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
impl StretchControls {
    /// The selected time-stretch backend.
    #[must_use]
    pub fn backend(&self) -> StretchKind {
        StretchKind::from(self.engine.backend.load(Ordering::Relaxed))
    }

    /// Whether key-lock (pitch-preserving tempo) is enabled.
    #[must_use]
    pub fn keylock(&self) -> bool {
        self.engine.keylock.load(Ordering::Relaxed)
    }

    /// Select the time-stretch backend.
    pub fn set_backend(&self, backend: StretchKind) {
        self.engine
            .backend
            .store(u8::from(backend), Ordering::Relaxed);
    }

    /// Enable/disable key-lock.
    pub fn set_keylock(&self, on: bool) {
        self.engine.keylock.store(on, Ordering::Relaxed);
    }
}
