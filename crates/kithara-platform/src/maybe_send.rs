#[cfg(not(target_arch = "wasm32"))]
pub use crate::native::maybe_send::*;
#[cfg(target_arch = "wasm32")]
pub use crate::wasm::maybe_send::*;
