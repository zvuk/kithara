#[cfg(not(target_arch = "wasm32"))]
pub use crate::native::tokio::runtime::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::tokio::runtime::*;
