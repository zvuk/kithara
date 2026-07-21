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
pub(crate) use elastic_renderer::ElasticPrepareError;
pub(crate) use feeder::PreparedElasticRenderer;
pub(in crate::player) use feeder::ReleasedPlayerResource;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use feeder::{ElasticPreparationPoll, PreparingElasticRenderer};
pub use feeder::{PlayerResource, ReadOutcome};
pub use read::TrackReadOutcome;
pub(crate) use read::TrackRenderMode;
