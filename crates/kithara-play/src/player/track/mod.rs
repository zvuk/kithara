mod core;
#[cfg(not(target_arch = "wasm32"))]
mod elastic;
#[cfg(not(target_arch = "wasm32"))]
mod elastic_renderer;
mod fade;
mod feeder;
mod read;
mod triggers;

pub use core::{PlayerTrack, TrackAxis, TrackParams};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use elastic::{ElasticPlanError, plan_elastic_segments};
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::player) use elastic_renderer::{
    Active, ElasticRenderOutcome, ElasticRenderer, Ready,
};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use elastic_renderer::{
    ElasticPreparationPoll, ElasticPrepareError, PreparingElasticRenderer,
};
pub(in crate::player) use feeder::ReleasedPlayerResource;
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::player) use feeder::{BoundResource, PlayerResourceKind};
pub use feeder::{PlayerResource, ReadOutcome};
pub use read::TrackReadOutcome;
pub(crate) use read::TrackRenderMode;
