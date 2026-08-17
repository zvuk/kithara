use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

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

/// Live controls published by callers and snapshotted off RT for the resident tempo stage.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    speed: Arc<AtomicF32>,
    region_plan: ArcSwapOption<RegionPlan>,
    engine: EngineControls,
    revision: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct StretchSnapshot {
    pub(crate) backend: StretchKind,
    pub(crate) keylock: bool,
    pub(crate) plan: Option<Arc<RegionPlan>>,
    pub(crate) revision: u64,
    pub(crate) speed: f32,
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
            revision: AtomicU64::new(0),
        })
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub(crate) fn try_snapshot(&self) -> Option<StretchSnapshot> {
        for _ in 0..2 {
            let revision = self.revision.load(Ordering::Acquire);
            let snapshot = StretchSnapshot {
                backend: self.backend(),
                keylock: self.keylock(),
                plan: self.region_plan(),
                revision,
                speed: self.speed(),
            };
            if self.revision.load(Ordering::Acquire) == revision {
                return Some(snapshot);
            }
        }
        None
    }

    #[must_use]
    pub fn backend(&self) -> StretchKind {
        StretchKind::from(self.engine.backend.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn keylock(&self) -> bool {
        self.engine.keylock.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn region_plan(&self) -> Option<Arc<RegionPlan>> {
        self.region_plan.load_full()
    }

    pub fn set_region_plan(&self, plan: Option<Arc<RegionPlan>>) {
        self.region_plan.store(plan);
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub fn set_backend(&self, backend: StretchKind) {
        self.engine
            .backend
            .store(u8::from(backend), Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub fn set_keylock(&self, on: bool) {
        self.engine.keylock.store(on, Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed, Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed.load(Ordering::Relaxed)
    }
}
