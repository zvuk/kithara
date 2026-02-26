#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
pub use crate::bindings::{build_info, setup};
#[cfg(target_arch = "wasm32")]
pub use crate::player::WasmPlayer;
