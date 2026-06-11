#[cfg(not(target_arch = "wasm32"))]
pub use crate::native::logging::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::logging::*;
