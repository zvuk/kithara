#[cfg(not(target_arch = "wasm32"))]
mod bound_plan;
mod core;
mod fade;
mod feeder;
mod read;
mod triggers;

pub use core::{PlayerTrack, TrackAxis, TrackParams};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use bound_plan::{ElasticPlanError, plan_elastic_anchor, plan_elastic_segments};
pub(in crate::player) use feeder::ReleasedPlayerResource;
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::player) use feeder::{BoundResource, PlayerResourceKind};
pub use feeder::{PlayerResource, ReadOutcome};
pub use read::TrackReadOutcome;
pub(crate) use read::TrackRenderMode;
