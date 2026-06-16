#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
mod bungee;
mod controls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod processor;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod region_plan;
#[cfg(all(test, not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
mod region_tests;
#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-signalsmith"))]
mod signalsmith;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod stretch_backend;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod stretch_factory;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod stretch_kind;

pub use controls::StretchControls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use {
    processor::TimeStretchProcessor,
    region_plan::{RegionPlan, RegionPlanError},
    stretch_backend::{StretchBackend, StretchBackendError},
    stretch_kind::StretchBackendKind,
};
