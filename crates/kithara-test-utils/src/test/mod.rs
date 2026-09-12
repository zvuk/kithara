mod tracing_init;
#[cfg(not(target_arch = "wasm32"))]
pub mod usdt;

pub use tracing_init::{init_tracing, setup_tracing, setup_tracing_with_filter};
