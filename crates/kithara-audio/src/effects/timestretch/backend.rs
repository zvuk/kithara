use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
pub use kithara_stretch::{StretchBackend, StretchBackendError, StretchKind};
use portable_atomic::AtomicF32;

pub use super::processor::TimeStretchProcessor;
use crate::region::RegionPlan;

#[derive(Debug)]
struct EngineControls {
    keylock: AtomicBool,
    backend: AtomicU8,
}

/// Live playback-speed control shared by the caller and the effect chain.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    speed: Arc<AtomicF32>,
    region_plan: ArcSwapOption<RegionPlan>,
    engine: EngineControls,
}

impl StretchControls {
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            speed: Arc::new(AtomicF32::new(speed)),
            region_plan: ArcSwapOption::const_empty(),
            engine: EngineControls {
                keylock: AtomicBool::new(false),
                backend: AtomicU8::new(u8::from(StretchKind::default())),
            },
        })
    }

    #[must_use]
    pub fn region_plan(&self) -> Option<Arc<RegionPlan>> {
        self.region_plan.load_full()
    }

    pub fn set_region_plan(&self, plan: Option<Arc<RegionPlan>>) {
        self.region_plan.store(plan);
    }

    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed, Ordering::Relaxed);
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn backend(&self) -> StretchKind {
        StretchKind::from(self.engine.backend.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn keylock(&self) -> bool {
        self.engine.keylock.load(Ordering::Relaxed)
    }

    pub fn set_backend(&self, backend: StretchKind) {
        self.engine
            .backend
            .store(u8::from(backend), Ordering::Relaxed);
    }

    pub fn set_keylock(&self, on: bool) {
        self.engine.keylock.store(on, Ordering::Relaxed);
    }
}
