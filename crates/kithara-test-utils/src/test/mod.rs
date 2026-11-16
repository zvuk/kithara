#[cfg(target_arch = "wasm32")]
mod browser_output;
mod clock_measurement;
#[cfg(any(not(target_arch = "wasm32"), feature = "subscriber"))]
mod tracing_init;
#[cfg(not(target_arch = "wasm32"))]
pub mod usdt;
#[cfg(not(target_arch = "wasm32"))]
mod wall;

#[cfg(target_arch = "wasm32")]
pub use browser_output::OutputStartupProbe;
pub use budget::TestBudget;
pub use clock_measurement::ClockMeasurement;
#[cfg(any(not(target_arch = "wasm32"), feature = "subscriber"))]
pub use tracing_init::{init_tracing, setup_tracing, setup_tracing_with_filter};
#[cfg(not(target_arch = "wasm32"))]
pub use wall::wall_sleep;
mod budget;
