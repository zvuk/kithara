mod controls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod processor;
#[cfg(all(
    test,
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod region_tests;

pub use controls::StretchControls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use {
    kithara_stretch::{StretchBackend, StretchBackendError, StretchBackendKind},
    processor::TimeStretchProcessor,
};

pub use crate::region::{RegionPlan, RegionPlanError};
