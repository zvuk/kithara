#![forbid(unsafe_code)]

#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;

#[cfg(target_arch = "wasm32")]
pub use crate::bindings::{build_info, setup};
#[cfg(target_arch = "wasm32")]
pub use crate::player::WasmPlayer;
