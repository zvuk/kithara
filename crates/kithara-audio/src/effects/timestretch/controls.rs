use std::sync::atomic::Ordering;

use arc_swap::ArcSwapOption;
use kithara_platform::sync::Arc;
use portable_atomic::AtomicF32;

use crate::region::RegionPlan;

/// Live playback-speed control shared by the caller and the effect chain.
#[derive(Debug)]
#[non_exhaustive]
pub struct StretchControls {
    speed: Arc<AtomicF32>,
    region_plan: ArcSwapOption<RegionPlan>,
}

impl StretchControls {
    #[must_use]
    pub fn new(speed: f32) -> Arc<Self> {
        Arc::new(Self {
            speed: Arc::new(AtomicF32::new(speed)),
            region_plan: ArcSwapOption::const_empty(),
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

    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed, Ordering::Relaxed);
    }

    #[must_use]
    pub fn speed(&self) -> f32 {
        self.speed.load(Ordering::Relaxed)
    }
}
