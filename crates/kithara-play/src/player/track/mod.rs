mod core;
#[cfg(not(target_arch = "wasm32"))]
mod elastic;
#[cfg(not(target_arch = "wasm32"))]
mod elastic_renderer;
#[cfg(not(target_arch = "wasm32"))]
mod elastic_source;
mod fade;
mod feeder;
mod read;
mod triggers;

pub use core::{PlayerTrack, TrackAxis, TrackParams};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use elastic_renderer::ElasticPreparationOutcome;
pub(crate) use feeder::PreparedElasticRenderer;
pub use feeder::{PlayerResource, ReadOutcome};
pub use read::TrackReadOutcome;
pub(crate) use read::TrackRenderMode;
