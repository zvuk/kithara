mod config;
pub(crate) mod lifecycle;
#[cfg(all(target_arch = "wasm32", feature = "uniffi-web"))]
mod tick;

#[cfg(not(target_arch = "wasm32"))]
pub use config::ensure_default_host;
pub use config::{default_host_config, initialize_host};
#[cfg(all(target_arch = "wasm32", feature = "uniffi-web"))]
pub use tick::tick_host;

pub use super::config::FfiHostConfig;
