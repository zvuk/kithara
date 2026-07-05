pub mod eq;
pub mod timestretch;

pub use eq::{EqBandConfig, EqEffect, FilterKind, IsolatorEq, generate_log_spaced_bands};
pub use timestretch::{RegionPlan, RegionPlanError, StretchControls};
#[cfg(all(
    not(target_arch = "wasm32"),
    any(feature = "stretch-signalsmith", feature = "stretch-bungee")
))]
pub use timestretch::{StretchBackend, StretchBackendError, StretchKind, TimeStretchProcessor};
