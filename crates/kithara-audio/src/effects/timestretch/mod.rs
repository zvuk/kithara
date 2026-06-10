#[cfg(all(not(target_arch = "wasm32"), feature = "stretch-bungee"))]
mod bungee;
mod controls;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
mod processor;
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
pub use processor::TimeStretchProcessor;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use stretch_backend::{StretchBackend, StretchBackendError};
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use stretch_kind::StretchBackendKind;
