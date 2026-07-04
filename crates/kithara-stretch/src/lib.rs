mod backend;
pub use backend::{StretchBackend, StretchBackendError};

mod region;
pub use region::{ActiveRegion, GridSegment, RegionPlan, RegionPlanError};

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod kind;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub use kind::StretchBackendKind;

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod factory;
#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
pub use factory::build_backend;

#[cfg(any(feature = "stretch-signalsmith", feature = "stretch-bungee"))]
mod backends;
