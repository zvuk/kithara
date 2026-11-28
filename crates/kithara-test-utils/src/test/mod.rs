#[cfg(target_arch = "wasm32")]
mod browser_output;
mod clock_measurement;
#[cfg(all(target_os = "linux", not(miri)))]
mod resident;
#[cfg(any(not(target_arch = "wasm32"), feature = "subscriber"))]
mod tracing_init;
#[cfg(not(target_arch = "wasm32"))]
pub mod usdt;
#[cfg(not(target_arch = "wasm32"))]
mod wall;

#[cfg(target_arch = "wasm32")]
pub use browser_output::OutputStartupProbe;
pub use budget::{TestBudget, timeout_for_clock};
pub use clock_measurement::ClockMeasurement;
#[cfg(all(target_os = "linux", not(miri)))]
pub use resident::make_binary_resident;
#[cfg(any(not(target_arch = "wasm32"), feature = "subscriber"))]
pub use tracing_init::{init_tracing, setup_tracing, setup_tracing_with_filter};
#[cfg(not(target_arch = "wasm32"))]
pub use wall::wall_sleep;
mod budget;
